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

use crate::platform::sync::{Arc, Mutex};
use bevy::prelude::*;
use bevy::tasks::IoTaskPool;

use crate::platform::model_file;
use crate::resources::{AvatarState, EditDialogState, GameState, OnlineSession, RightPanel};
use crate::systems::egui_manager::EguiManager;
use crate::systems::model_assets;
use crate::systems::{icons, theme};

/// What the store accepts, said before the file dialog opens.
///
/// The limits are the server's and are enforced there, for everyone, because a
/// client that skipped them could not be detected. Saying them here is so a
/// person exports the right thing, not so anything is decided here — which is
/// why the numbers are read from `cartopolis_core::model_limits`, the constants
/// `simulator::assets` enforces, rather than typed a second time.
fn upload_format_hint() -> String {
    use cartopolis_core::model_limits::{
        with_thousands, MAX_ASSET_MB, MAX_NODES, MAX_TEXTURE_EDGE, MAX_TRIANGLES,
    };
    format!(
        "glTF: .glb, or a .gltf with everything embedded in it (the server repacks \
         that one). A file that points at a .bin or a texture beside it is refused, \
         not fetched. Up to {MAX_ASSET_MB} MB, {} triangles, {} nodes, \
         {MAX_TEXTURE_EDGE} px textures.",
        with_thousands(MAX_TRIANGLES),
        with_thousands(MAX_NODES as u64),
    )
}

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
    /// Whether this upload goes straight into the shared library. Asked here
    /// rather than as a second visit to the list, and forced off for a licence
    /// that may not be handed on.
    pub upload_share: bool,
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

/// Upload a model, and publish it in the same breath when that is what was asked
/// for.
///
/// **One action, not two.** Uploading and sharing stay separate routes on the
/// server — a person may upload something they are not allowed to hand on — but
/// putting a model in the library was two trips through the panel: upload, wait
/// for the list to come back, find the row, press Share. The question that second
/// trip asks is the one already answered at upload, where the licence is stated,
/// so it is asked there and the chain runs here.
///
/// Chained inside the same task rather than queued as a second request, because
/// it needs the inventory id the upload answers with.
fn upload(
    inbox: Inbox,
    bytes: Vec<u8>,
    name: String,
    licence: Licence,
    attribution: String,
    share_too: bool,
) {
    IoTaskPool::get()
        .spawn(async move {
            let query = format!(
                "name={}&licence={}&attribution={}",
                encode(&name),
                licence.as_str(),
                encode(&attribution)
            );
            let reply = match model_assets::api_upload(&query, bytes).await {
                Ok(body) if share_too && licence.may_share() => {
                    match uploaded_item_id(&body) {
                        Some(id) => {
                            let payload = format!(r#"{{"name":{}}}"#, json_string(&name));
                            match model_assets::api_post(
                                &format!("/api/inventory/{id}/share"),
                                Some(payload),
                            )
                            .await
                            {
                                Ok(_) => Reply::Done(format!("Uploaded “{name}” and shared it.")),
                                // The file landed either way, so this says which
                                // half happened rather than reading as though the
                                // whole thing failed.
                                Err(e) => Reply::Failed(format!(
                                    "Uploaded “{name}”, but sharing it: {}",
                                    server_message(&e)
                                )),
                            }
                        }
                        None => Reply::Done(format!(
                            "Uploaded “{name}”, but the server did not say which item it is, \
                             so it is not shared yet."
                        )),
                    }
                }
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

/// The inventory id out of what `POST /api/assets` answers with — the created
/// item, in the same shape the listing carries.
fn uploaded_item_id(body: &str) -> Option<i64> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["id"].as_i64())
}

/// Publish a model to everyone on this server.
///
/// The name travels and the **category does not**: nothing in this panel picks
/// one, and sending a constant would put "other" on every entry as though somebody
/// had chosen it. Categorising is a curator's job, in the atlas web UI, and the server
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
                // Said rather than silently cleared. On Linux the file chooser is
                // `org.freedesktop.portal.FileChooser` over DBus, and a desktop
                // with no portal running answers exactly as a person who pressed
                // Cancel does — so "nothing happened" needs a line either way, or
                // the button reads as broken.
                state.status = "No file chosen.".into();
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
    //
    // **Two reasons, two sentences.** These used to be one flag, and a signed-in
    // person whose server never said where its model store is was told to sign
    // in — which is not something they can act on, and made a working panel look
    // broken. A server can be joined and still have no HTTP surface (no
    // `--http-listen`) or one the client cannot derive, and that is the
    // operator's to fix, not the player's.
    let have_session = online.is_some();
    let have_store = model_assets::have_endpoint();
    let signed_in = have_session && have_store;

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
    let mut upload_share = state.upload_share;
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
                let message = if have_session {
                    "This server did not say where its model store is, so there is nothing \
                     to show. An operator turns one on with --http-listen, or names it with \
                     api_base_url."
                } else {
                    "Models live on a simulator. Sign in to a server to see yours."
                };
                ui.label(egui::RichText::new(message).color(theme::TEXT_DIM));
                return;
            }

            // Refresh first, from the right, so the two tabs take what is left.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(icons::REFRESH)
                    .on_hover_text("fetch both lists again")
                    .clicked()
                {
                    want_refresh = true;
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    theme::segmented(
                        ui,
                        &mut tab,
                        &[(Tab::Mine, "Mine"), (Tab::Library, "Library")],
                    );
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
                            if upload_licence.may_share() {
                                ui.checkbox(
                                    &mut upload_share,
                                    "Share it with everyone on this server",
                                );
                            } else {
                                // Not merely disabled: a checkbox left ticked
                                // from a previous answer would be a promise the
                                // server is about to refuse.
                                upload_share = false;
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
                        } else {
                            if ui.button("Add a model…").clicked() {
                                want_pick = true;
                            }
                            // What to export before opening the dialog, because a
                            // refusal after a 12 MB upload is a slow way to learn
                            // it. The numbers are the server's
                            // (`cartopolis_core::model_limits`, enforced in
                            // `simulator::assets`) and this says the same thing
                            // its refusals do.
                            ui.label(
                                egui::RichText::new(upload_format_hint())
                                    .small()
                                    .color(theme::TEXT_DIM),
                            );
                        }
                    } else {
                        // Android: no document picker (`platform::model_file`).
                        // Said, rather than left as a panel that silently cannot
                        // do the thing the panel is named after.
                        ui.label(
                            egui::RichText::new(
                                "Adding a model needs a desktop or a browser. \
                                 Anything in the library can be placed from here.",
                            )
                            .small()
                            .color(theme::TEXT_DIM),
                        );
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
    state.upload_share = upload_share;
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
                state.status = "Choose a model…".into();
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
            let share_too = state.upload_share && state.upload_licence.may_share();
            upload(
                state.inbox.clone(),
                bytes,
                name,
                state.upload_licence,
                state.upload_attribution.clone(),
                share_too,
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

    /// The hint says the server's numbers, grouped the way a person reads them,
    /// and names both forms the store takes.
    #[test]
    fn the_upload_hint_states_the_servers_limits() {
        use cartopolis_core::model_limits::{with_thousands, MAX_ASSET_MB, MAX_TRIANGLES};
        let hint = upload_format_hint();
        assert!(hint.contains(".glb") && hint.contains(".gltf"), "{hint}");
        assert!(hint.contains(&format!("{MAX_ASSET_MB} MB")), "{hint}");
        assert!(
            hint.contains(&format!("{} triangles", with_thousands(MAX_TRIANGLES))),
            "{hint}"
        );
    }

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

    /// Publishing on upload needs the id the upload answered with; it is the
    /// created inventory item, in the listing's own shape.
    #[test]
    fn reads_the_new_items_id() {
        let body = r#"{"id":42,"name":"Bench","asset":"aa","triangles":812,"licence":"cc0"}"#;
        assert_eq!(uploaded_item_id(body), Some(42));
    }

    /// An answer this does not recognise must not be guessed at — the upload
    /// still happened, and the panel says the sharing half did not.
    #[test]
    fn an_unrecognised_answer_has_no_id() {
        assert_eq!(uploaded_item_id("created"), None);
        assert_eq!(uploaded_item_id(r#"{"asset":"aa"}"#), None);
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
