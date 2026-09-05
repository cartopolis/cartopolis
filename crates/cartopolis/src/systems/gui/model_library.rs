//! The Models panel: what you hold, what the server shares, and putting one in
//! the world.
//!
//! # Two lists, one file store
//!
//! **Mine** is the inventory — a person's named references to files they uploaded
//! or took a copy of. **Library** is the shared catalogue, which is per *file*: a
//! model somebody published, with a category and a licence, that everybody can
//! use. The same bench can be in both, and that is not a duplicate — the file is
//! one file, and the two lists answer different questions ("what have I got?" and
//! "what is there?").
//!
//! # Sharing asks a question, and the question is not optional
//!
//! Using a model in a world and handing copies of it to other people are
//! different acts under most asset licences — Quaternius, whose characters this
//! project's own avatars come from, moved to a licence in August 2026 that permits
//! the first and forbids the second in as many words. So the licence is asked at
//! upload, recorded against the uploader, and a model claimed as "something else"
//! can be used and can never be shared. The server enforces it; this panel only
//! has to not lie about it.
//!
//! # Every request here is a detached fetch
//!
//! The model API is HTTP, not the wire (`systems::model_assets`), so a click
//! spawns a task on the `IoTaskPool` that pushes its answer into a queue this
//! panel drains — the same shape `geo_nav`'s geocoding uses, and for the same
//! reason: it has to work on the web, where there are no OS threads.

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use std::sync::{Arc, Mutex};

use crate::platform::model_file;
use crate::resources::{AvatarState, EditDialogState, GameState, OnlineSession, RightPanel};
use crate::systems::egui_manager::EguiManager;
use crate::systems::model_assets;
use crate::systems::{icons, theme};

/// One line of somebody's inventory, as the server describes it.
#[derive(Debug, Clone, Default)]
pub struct InventoryItem {
    pub id: i64,
    pub name: String,
    pub folder: String,
    pub asset: String,
    pub triangles: u64,
    pub licence: String,
    /// `Some(visibility)` when this file is in the shared library.
    pub shared: Option<String>,
}

/// One entry in the shared catalogue.
#[derive(Debug, Clone, Default)]
pub struct LibraryEntry {
    pub asset: String,
    pub name: String,
    pub category: String,
    pub licence: String,
    pub attribution: String,
    pub triangles: u64,
}

/// What a finished request has to say. A queue and not a slot, because several
/// can be outstanding and the one that answers first must not overwrite an answer
/// nothing has read yet.
enum Reply {
    Inventory(Vec<InventoryItem>),
    Library(Vec<LibraryEntry>),
    /// A sentence for the status line, and a nudge to refresh.
    Done(String),
    Failed(String),
}

type Inbox = Arc<Mutex<Vec<Reply>>>;

/// Which list the panel is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Mine,
    Library,
}

/// What an uploader says about where a model came from.
///
/// Mirrors the server's `library::Licence` — it is the same four answers, because
/// the string travels straight to it. Kept as a client-side enum rather than a
/// free-text field so the question is answered by choosing, not by typing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Licence {
    #[default]
    Own,
    Cc0,
    CcBy,
    Other,
}

impl Licence {
    fn as_str(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::Cc0 => "cc0",
            Self::CcBy => "cc-by",
            Self::Other => "other",
        }
    }

    /// What the person choosing sees. Written as a claim they are making, because
    /// that is what it is.
    fn label(self) -> &'static str {
        match self {
            Self::Own => "I made it",
            Self::Cc0 => "CC0 — public domain",
            Self::CcBy => "CC BY — credit required",
            Self::Other => "Something else",
        }
    }

    /// Whether choosing this permits sharing. Advisory here; the server refuses.
    fn may_share(self) -> bool {
        !matches!(self, Self::Other)
    }

    /// Whether a credit line has to be given with it. Asked at upload, because
    /// that is the moment the person still has the answer in front of them —
    /// chasing it up later means chasing it up never.
    fn needs_attribution(self) -> bool {
        matches!(self, Self::CcBy)
    }
}

#[derive(Resource, Default)]
pub struct ModelLibraryState {
    pub tab: Tab,
    pub items: Vec<InventoryItem>,
    pub entries: Vec<LibraryEntry>,
    pub search: String,
    /// The last thing that happened, shown at the foot of the panel.
    pub status: String,
    /// Set while at least one request is outstanding, so the panel can say so
    /// rather than looking like it ignored the click.
    pub busy: bool,
    /// Cleared when a session comes up, so opening the panel fetches once.
    loaded: bool,
    inbox: Inbox,
    /// A file the user is choosing, and then the details they are asked for.
    pick: Option<model_file::Pick>,
    pending_bytes: Option<Vec<u8>>,
    pub upload_name: String,
    pub upload_licence: Licence,
    pub upload_attribution: String,
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

fn fetch_inventory(inbox: Inbox) {
    IoTaskPool::get()
        .spawn(async move {
            let reply = match model_assets::api_get("/api/inventory").await {
                Ok(body) => match parse_items(&body) {
                    Ok(items) => Reply::Inventory(items),
                    Err(e) => Reply::Failed(e),
                },
                Err(e) => Reply::Failed(e),
            };
            push(&inbox, reply);
        })
        .detach();
}

fn fetch_library(inbox: Inbox) {
    IoTaskPool::get()
        .spawn(async move {
            let reply = match model_assets::api_get("/api/library").await {
                Ok(body) => match parse_entries(&body) {
                    Ok(entries) => Reply::Library(entries),
                    Err(e) => Reply::Failed(e),
                },
                Err(e) => Reply::Failed(e),
            };
            push(&inbox, reply);
        })
        .detach();
}

fn upload(inbox: Inbox, bytes: Vec<u8>, name: String, licence: Licence, attribution: String) {
    IoTaskPool::get()
        .spawn(async move {
            let query = format!(
                "name={}&licence={}&attribution={}",
                encode(&name),
                licence.as_str(),
                encode(&attribution)
            );
            let reply = match model_assets::api_upload(&query, bytes).await {
                Ok(_) => Reply::Done(format!("Uploaded “{name}”.")),
                // The server's refusals are written to be shown — they say what
                // was measured and what the limit is — so they are passed
                // through rather than replaced with a generic failure.
                Err(e) => Reply::Failed(server_message(&e)),
            };
            push(&inbox, reply);
        })
        .detach();
}

/// Publish a model to everyone on this server.
///
/// The name travels and the **category does not**: nothing in this panel picks
/// one, and sending a constant would put "other" on every entry as though somebody
/// had chosen it. Categorising is a curator's job, in cartopy, and the server
/// defaults the field for exactly that reason.
fn share(inbox: Inbox, id: i64, name: String) {
    IoTaskPool::get()
        .spawn(async move {
            let body = format!(r#"{{"name":{}}}"#, json_string(&name));
            let reply =
                match model_assets::api_post(&format!("/api/inventory/{id}/share"), Some(body))
                    .await
                {
                    Ok(_) => Reply::Done(format!("Shared “{name}”.")),
                    Err(e) => Reply::Failed(server_message(&e)),
                };
            push(&inbox, reply);
        })
        .detach();
}

fn unshare(inbox: Inbox, id: i64, name: String) {
    IoTaskPool::get()
        .spawn(async move {
            let reply = match model_assets::api_delete(&format!("/api/inventory/{id}/share")).await
            {
                Ok(()) => Reply::Done(format!("Took “{name}” out of the library.")),
                Err(e) => Reply::Failed(server_message(&e)),
            };
            push(&inbox, reply);
        })
        .detach();
}

fn discard(inbox: Inbox, id: i64, name: String) {
    IoTaskPool::get()
        .spawn(async move {
            let reply = match model_assets::api_delete(&format!("/api/inventory/{id}")).await {
                Ok(()) => Reply::Done(format!("Removed “{name}”.")),
                Err(e) => Reply::Failed(server_message(&e)),
            };
            push(&inbox, reply);
        })
        .detach();
}

fn take(inbox: Inbox, hash: String, name: String) {
    IoTaskPool::get()
        .spawn(async move {
            let reply =
                match model_assets::api_post(&format!("/api/library/{hash}/take"), None).await {
                    Ok(_) => Reply::Done(format!("“{name}” is in your inventory.")),
                    Err(e) => Reply::Failed(server_message(&e)),
                };
            push(&inbox, reply);
        })
        .detach();
}

fn push(inbox: &Inbox, reply: Reply) {
    if let Ok(mut queue) = inbox.lock() {
        queue.push(reply);
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn parse_items(body: &str) -> Result<Vec<InventoryItem>, String> {
    let json: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = json.as_array().ok_or("unexpected inventory response")?;
    Ok(rows
        .iter()
        .map(|row| InventoryItem {
            id: row["id"].as_i64().unwrap_or(0),
            name: row["name"].as_str().unwrap_or("Unnamed").to_string(),
            folder: row["folder"].as_str().unwrap_or("").to_string(),
            asset: row["asset"].as_str().unwrap_or("").to_string(),
            triangles: row["triangles"].as_u64().unwrap_or(0),
            licence: row["licence"].as_str().unwrap_or("other").to_string(),
            shared: row["shared"].as_str().map(str::to_string),
        })
        .filter(|item| !item.asset.is_empty())
        .collect())
}

fn parse_entries(body: &str) -> Result<Vec<LibraryEntry>, String> {
    let json: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = json.as_array().ok_or("unexpected library response")?;
    Ok(rows
        .iter()
        .map(|row| LibraryEntry {
            asset: row["asset"].as_str().unwrap_or("").to_string(),
            name: row["name"].as_str().unwrap_or("Unnamed").to_string(),
            category: row["category"].as_str().unwrap_or("other").to_string(),
            licence: row["licence"].as_str().unwrap_or("other").to_string(),
            attribution: row["attribution"].as_str().unwrap_or("").to_string(),
            triangles: row["triangles"].as_u64().unwrap_or(0),
        })
        .filter(|entry| !entry.asset.is_empty())
        .collect())
}

/// Pull the server's own sentence out of an error, when there is one.
///
/// `platform::http` flattens a failed status into a string, and the body it
/// carries is the `{"error": "..."}` the API answers with. Showing the raw
/// wrapper would put "HTTP 400 Bad Request" in front of someone whose actual
/// problem is that their model has 400,000 triangles.
fn server_message(raw: &str) -> String {
    if let Some(start) = raw.find(r#"{"error""#) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw[start..]) {
            if let Some(message) = json["error"].as_str() {
                return message.to_string();
            }
        }
    }
    raw.to_string()
}

/// Percent-encode a query value.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// How a stored licence name reads to a person.
///
/// The server stores the four codes; showing "cc-by" in a list is showing a
/// database value to somebody who wants to know whether they have to credit
/// anyone.
fn licence_label(licence: &str) -> &'static str {
    match licence {
        "own" => "by its author",
        "cc0" => "CC0",
        "cc-by" => "CC BY",
        _ => "licence unknown",
    }
}

/// A JSON string literal, escaping what has to be escaped.
fn json_string(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

/// Drain finished requests and the file picker.
///
/// Separate from the panel because both outlive it being open: an upload started
/// and then the panel closed must still land, and a file dialog the user is
/// staring at is not a reason to keep a panel on screen.
pub fn poll_model_library(mut state: ResMut<ModelLibraryState>) {
    if let Some(pick) = state.pick.as_mut() {
        match pick.poll() {
            model_file::Outcome::Pending => {}
            model_file::Outcome::Ready(bytes) => {
                state.pick = None;
                let size = bytes.len();
                state.pending_bytes = Some(bytes);
                state.status = format!("Chosen: {} KB. Say what it is, then Upload.", size / 1024);
            }
            model_file::Outcome::Cancelled => {
                state.pick = None;
                state.status.clear();
            }
            model_file::Outcome::Failed(e) => {
                state.pick = None;
                state.status = e;
            }
        }
    }

    let replies: Vec<Reply> = match state.inbox.lock() {
        Ok(mut queue) => queue.drain(..).collect(),
        Err(_) => Vec::new(),
    };
    if replies.is_empty() {
        return;
    }
    state.busy = false;
    let mut refresh = false;
    for reply in replies {
        match reply {
            Reply::Inventory(items) => state.items = items,
            Reply::Library(entries) => state.entries = entries,
            Reply::Done(message) => {
                state.status = message;
                refresh = true;
            }
            Reply::Failed(message) => state.status = message,
        }
    }
    if refresh {
        // Anything that changed the world's idea of what exists invalidates both
        // lists: sharing moves a model between them, and taking a copy adds to
        // one of them.
        fetch_inventory(state.inbox.clone());
        fetch_library(state.inbox.clone());
        state.busy = true;
    }
}

/// The panel.
#[allow(clippy::too_many_lines)]
pub fn render_model_library_panel(
    mut egui: ResMut<EguiManager>,
    mut right_panel: ResMut<RightPanel>,
    mut state: ResMut<ModelLibraryState>,
    mut game_state: ResMut<GameState>,
    online: Option<Res<OnlineSession>>,
    avatar_state: Res<AvatarState>,
) {
    if !right_panel.is(RightPanel::Models) {
        return;
    }
    // Offline there is no simulator, so there is no store and no inventory. The
    // panel says so rather than showing two empty lists that look like a failure.
    let signed_in = online.is_some() && model_assets::have_endpoint();

    if signed_in && !state.loaded {
        state.loaded = true;
        state.busy = true;
        fetch_inventory(state.inbox.clone());
        fetch_library(state.inbox.clone());
    }

    // The panel body cannot borrow `state`: `right_panel` already holds a mutable
    // borrow of the egui manager and the closure would need a second of the
    // resource it is called from. So the edited fields are moved out, worked on,
    // and put back — which also makes every action below a plain value rather
    // than a mutation happening inside a layout pass.
    let mut tab = state.tab;
    let mut search = std::mem::take(&mut state.search);
    let mut upload_name = std::mem::take(&mut state.upload_name);
    let mut upload_attribution = std::mem::take(&mut state.upload_attribution);
    let mut upload_licence = state.upload_licence;
    let items = std::mem::take(&mut state.items);
    let entries = std::mem::take(&mut state.entries);
    let busy = state.busy;
    let status = state.status.clone();
    let has_bytes = state.pending_bytes.is_some();

    let mut want_pick = false;
    let mut want_upload = false;
    let mut want_refresh = false;
    let mut place: Option<(String, String)> = None;
    let mut share_item: Option<(i64, String)> = None;
    let mut unshare_item: Option<(i64, String)> = None;
    let mut discard_item: Option<(i64, String)> = None;
    let mut take_entry: Option<(String, String)> = None;

    crate::systems::egui_manager::right_panel(
        &mut egui,
        &mut right_panel,
        RightPanel::Models,
        "model_library_panel",
        360.0,
        |ui| {
            if !signed_in {
                ui.label(
                    egui::RichText::new(
                        "Models live on a simulator. Sign in to a server to see yours.",
                    )
                    .color(theme::TEXT_DIM),
                );
                return;
            }

            ui.horizontal(|ui| {
                ui.selectable_value(&mut tab, Tab::Mine, "Mine");
                ui.selectable_value(&mut tab, Tab::Library, "Library");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(icons::REFRESH)
                        .on_hover_text("fetch both lists again")
                        .clicked()
                    {
                        want_refresh = true;
                    }
                });
            });
            ui.add_space(4.0);
            ui.add(egui::TextEdit::singleline(&mut search).hint_text("Search"));
            ui.add_space(4.0);
            ui.separator();

            let needle = search.to_lowercase();
            // The upload block and the status line are laid out first, from the
            // bottom, so the list takes what is left: a refusal that arrives while
            // you are reading must not push the list out from under you. Same rule
            // the script editor's console follows.
            egui::Panel::bottom("model_library_actions")
                .show_separator_line(false)
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    ui.separator();
                    if model_file::is_available() {
                        if has_bytes {
                            ui.add(
                                egui::TextEdit::singleline(&mut upload_name)
                                    .hint_text("What is it?"),
                            );
                            ui.label(
                                egui::RichText::new("Where did it come from?")
                                    .color(theme::TEXT_DIM),
                            );
                            for licence in
                                [Licence::Own, Licence::Cc0, Licence::CcBy, Licence::Other]
                            {
                                ui.radio_value(&mut upload_licence, licence, licence.label());
                            }
                            if upload_licence.needs_attribution() {
                                ui.add(
                                    egui::TextEdit::singleline(&mut upload_attribution)
                                        .hint_text("Credit line, e.g. “Bench by A. N. Other”"),
                                );
                            }
                            if !upload_licence.may_share() {
                                ui.label(
                                    egui::RichText::new(
                                        "You will be able to use this, but not share it.",
                                    )
                                    .small()
                                    .color(theme::TEXT_DIM),
                                );
                            }
                            if ui.button("Upload").clicked() {
                                want_upload = true;
                            }
                        } else if ui.button("Add a model…").clicked() {
                            want_pick = true;
                        }
                    }
                    if busy {
                        ui.label(egui::RichText::new("Working…").color(theme::TEXT_DIM));
                    } else if !status.is_empty() {
                        ui.label(egui::RichText::new(&status).color(theme::TEXT_DIM));
                    }
                });

            egui::ScrollArea::vertical().show(ui, |ui| match tab {
                Tab::Mine => {
                    if items.is_empty() {
                        ui.label(egui::RichText::new("Nothing here yet.").color(theme::TEXT_DIM));
                    }
                    for item in &items {
                        if !needle.is_empty() && !item.name.to_lowercase().contains(&needle) {
                            continue;
                        }
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new(&item.name)
                                    .strong()
                                    .color(theme::ACCENT_TEXT),
                            );
                            ui.label(
                                egui::RichText::new(format!("{} tri", item.triangles))
                                    .color(theme::TEXT_DIM),
                            );
                            if !item.folder.is_empty() {
                                ui.label(
                                    egui::RichText::new(&item.folder)
                                        .small()
                                        .color(theme::TEXT_DIM),
                                );
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("Place").clicked() {
                                place = Some((item.asset.clone(), item.name.clone()));
                            }
                            if item.shared.is_some() {
                                if ui
                                    .button("Unshare")
                                    .on_hover_text(
                                        "take it out of the shared library; anyone who \
                                         already took a copy keeps theirs",
                                    )
                                    .clicked()
                                {
                                    unshare_item = Some((item.id, item.name.clone()));
                                }
                            } else {
                                // The licence recorded at upload decides. The
                                // server refuses either way; the panel's job is
                                // not to offer a button that is going to be
                                // refused, and to say why it is not offering it.
                                let allowed = item.licence != "other";
                                let button = ui.add_enabled(allowed, egui::Button::new("Share"));
                                let button = if allowed {
                                    button.on_hover_text("publish it to everyone on this server")
                                } else {
                                    button.on_hover_text(
                                        "you said this came from somewhere else, so it \
                                         cannot be handed on",
                                    )
                                };
                                if button.clicked() {
                                    share_item = Some((item.id, item.name.clone()));
                                }
                            }
                            if ui
                                .button(icons::DELETE)
                                .on_hover_text("remove it from your inventory")
                                .clicked()
                            {
                                discard_item = Some((item.id, item.name.clone()));
                            }
                        });
                        ui.separator();
                    }
                }
                Tab::Library => {
                    if entries.is_empty() {
                        ui.label(
                            egui::RichText::new("Nobody has shared a model yet.")
                                .color(theme::TEXT_DIM),
                        );
                    }
                    for entry in &entries {
                        if !needle.is_empty() && !entry.name.to_lowercase().contains(&needle) {
                            continue;
                        }
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new(&entry.name)
                                    .strong()
                                    .color(theme::ACCENT_TEXT),
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} · {} tri · {}",
                                    entry.category,
                                    entry.triangles,
                                    licence_label(&entry.licence)
                                ))
                                .color(theme::TEXT_DIM),
                            );
                        });
                        // The credit line travels with the model. A CC-BY licence
                        // requires that, and a list of models is where a person
                        // meets one.
                        if !entry.attribution.is_empty() {
                            ui.label(
                                egui::RichText::new(&entry.attribution)
                                    .small()
                                    .color(theme::TEXT_DIM),
                            );
                        }
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("Place").clicked() {
                                place = Some((entry.asset.clone(), entry.name.clone()));
                            }
                            if ui
                                .button("Keep")
                                .on_hover_text("copy it into your inventory")
                                .clicked()
                            {
                                take_entry = Some((entry.asset.clone(), entry.name.clone()));
                            }
                        });
                        ui.separator();
                    }
                }
            });
        },
    );

    state.tab = tab;
    state.search = search;
    state.upload_name = upload_name;
    state.upload_attribution = upload_attribution;
    state.upload_licence = upload_licence;
    state.items = items;
    state.entries = entries;

    if want_refresh {
        state.busy = true;
        fetch_inventory(state.inbox.clone());
        fetch_library(state.inbox.clone());
    }
    if want_pick {
        match model_file::start() {
            Ok(pick) => {
                state.pick = Some(pick);
                state.status = "Choose a .glb file…".into();
            }
            Err(e) => state.status = e,
        }
    }
    if want_upload {
        if let Some(bytes) = state.pending_bytes.take() {
            let name = if state.upload_name.trim().is_empty() {
                "Unnamed model".to_string()
            } else {
                state.upload_name.clone()
            };
            state.busy = true;
            state.status = "Uploading…".into();
            upload(
                state.inbox.clone(),
                bytes,
                name,
                state.upload_licence,
                state.upload_attribution.clone(),
            );
            state.upload_name.clear();
            state.upload_attribution.clear();
        }
    }
    if let Some((id, name)) = share_item {
        state.busy = true;
        share(state.inbox.clone(), id, name);
    }
    if let Some((id, name)) = unshare_item {
        state.busy = true;
        unshare(state.inbox.clone(), id, name);
    }
    if let Some((id, name)) = discard_item {
        state.busy = true;
        discard(state.inbox.clone(), id, name);
    }
    if let Some((hash, name)) = take_entry {
        state.busy = true;
        take(state.inbox.clone(), hash, name);
    }
    if let Some((hash, name)) = place {
        place_model(&mut game_state, &avatar_state, &hash, &name);
        state.status = format!("Placed \u{201c}{name}\u{201d}.");
    }
}

/// Ask for a prim wearing this model, a couple of metres in front of the avatar.
///
/// Goes through `GameState::pending_prim_save`, which is the one path that turns
/// an edit-dialog state into a `CreatePrim` — so placing a model is the same
/// create every other object goes through, with one field set. Inventing a second
/// send site is how the two would come to disagree about geo anchoring.
fn place_model(game_state: &mut GameState, avatar_state: &AvatarState, hash: &str, name: &str) {
    let facing = avatar_state.facing_yaw;
    let ahead = Vec3::new(-facing.sin(), 0.0, -facing.cos()) * 2.0;
    let at = avatar_state.position + ahead;
    game_state.pending_prim_save = Some(EditDialogState {
        is_new: true,
        prim_id: None,
        name: name.to_string(),
        // A model replaces the procedural shape, but the field still travels and
        // the server still stores it — so it says the honest thing rather than an
        // empty string the shape parser would have to guess at.
        shape: "box".to_string(),
        model: Some(hash.to_string()),
        position: [at.x, at.y, at.z],
        rotation: [0.0, 0.0, 0.0],
        scale: [1.0, 1.0, 1.0],
        color: [1.0, 1.0, 1.0],
        ..Default::default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list the server sends is the list the panel shows, including the two
    /// fields a row is useless without.
    #[test]
    fn parses_an_inventory_listing() {
        let body = r#"[
            {"id":7,"name":"Bench","folder":"","asset":"aa","triangles":812,
             "byte_size":4096,"licence":"cc0","shared":"curated","created_at":"now"}
        ]"#;
        let items = parse_items(body).expect("a listing");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, 7);
        assert_eq!(items[0].triangles, 812);
        assert_eq!(items[0].shared.as_deref(), Some("curated"));
    }

    /// A row with no asset hash is dropped rather than drawn: it would offer a
    /// Place button that could only make a prim nothing can render.
    #[test]
    fn drops_a_row_with_no_asset() {
        let items = parse_items(r#"[{"id":1,"name":"Broken"}]"#).expect("a listing");
        assert!(items.is_empty());
    }

    /// The server's refusals are written to be read by the person who tried, so
    /// the wrapper around them has to come off.
    #[test]
    fn unwraps_the_servers_own_message() {
        let raw = r#"HTTP 400 Bad Request: {"error":"that model has 900000 triangles and the limit is 250000"}"#;
        assert_eq!(
            server_message(raw),
            "that model has 900000 triangles and the limit is 250000"
        );
    }

    /// A network failure has no JSON in it and must survive unchanged.
    #[test]
    fn leaves_a_plain_error_alone() {
        assert_eq!(server_message("connection refused"), "connection refused");
    }

    /// Only a licence that permits redistribution offers Share. The panel is not
    /// where this is enforced — the server refuses — but it must not offer a
    /// button that is going to be refused.
    #[test]
    fn other_may_not_be_shared() {
        assert!(Licence::Own.may_share());
        assert!(Licence::Cc0.may_share());
        assert!(Licence::CcBy.may_share());
        assert!(!Licence::Other.may_share());
    }
}
