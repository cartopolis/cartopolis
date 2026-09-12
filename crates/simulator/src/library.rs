//! Uploading a model, keeping it in an inventory, and publishing it to the
//! shared library.
//!
//! # Why this is HTTP and not the wire
//!
//! The same reason account management is (`http`'s note): an operator or
//! authoring action must not run an authorisation decision through the path a
//! movement intent takes, and the *admin* surface changing must not move every
//! client's protocol version. A model is also megabytes where the wire's whole
//! design is a frame per tick — putting one on `NetMessage` would sit a 12 MB
//! upload in front of everybody's next snapshot.
//!
//! What the wire does carry is the one field this exists to fill:
//! `PrimDto::model`, an asset hash. The bytes travel over HTTP, cached forever
//! because a file named by its own content can never change; the reference
//! travels in the snapshot, twenty times a second, as 32 bytes of hex.
//!
//! # The four tables, and the case that forces them apart
//!
//! `V16__model_assets.sql` has the schema and the argument. In short: the file is
//! deduplicated by content, so Alice and Bob uploading the same bench share one
//! file, and therefore "who uploaded it", "what is it called" and "is it shared"
//! cannot be columns on it.
//!
//! # What a refusal says
//!
//! Every error here is written to be shown to the person who tried. A model that
//! is too heavy says how heavy it is and what the limit is, because "rejected" is
//! a message that sends someone to a maintainer.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use bytes::Bytes;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::assets;
use crate::http::{internal, require_admin, require_session, ApiError, ApiResult, HttpState};

/// The routes, mounted by `http::router` alongside the authoring surface.
///
/// Note what is **not** under `/api/admin`: uploading, holding an inventory and
/// sharing are things any signed-in account does, so an admin token is neither
/// required nor sufficient. Only curation is an operator action.
///
/// And note what nothing here may be called: `/api/pipeline/*` belongs to
/// cartopy's own server, and the reverse proxy in front of both matches it first,
/// so a route named under that prefix would never be reached.
pub fn routes() -> Router<HttpState> {
    Router::new()
        .route(
            "/api/assets",
            // The body limit is raised **on this route alone**. Axum's default is
            // two megabytes, which would refuse almost every real model with a
            // message about the body rather than about the model — and raising it
            // globally would apply it to every JSON endpoint on the server, where
            // two megabytes is already far more than any of them should accept.
            post(upload_asset).layer(axum::extract::DefaultBodyLimit::max(
                assets::MAX_ASSET_BYTES,
            )),
        )
        .route("/api/assets/{name}", get(download_asset))
        .route("/api/inventory", get(list_inventory))
        .route(
            "/api/inventory/{id}",
            patch(update_item).delete(delete_item),
        )
        .route("/api/inventory/{id}/give", post(give_item))
        .route(
            "/api/inventory/{id}/share",
            post(share_item).delete(unshare_item),
        )
        .route("/api/library", get(browse_library))
        // Under `/api/library` rather than a prefix of its own, and that is not
        // cosmetic: `cartopolis.org` proxies three named prefixes to this server
        // and nothing else (`model-assets-and-inventory.md`), so a fourth would
        // be a Caddy change on the host before any client could read it.
        .route("/api/library/placeholders", get(list_placeholders))
        .route("/api/library/{hash}/take", post(take_from_library))
        .route(
            "/api/admin/library/placeholders/{kind}",
            put(set_placeholder).delete(clear_placeholder),
        )
        .route("/api/admin/library", get(admin_list_library))
        .route("/api/admin/library/{hash}", patch(curate_entry))
        .route("/api/admin/assets", get(admin_list_assets))
        .route("/api/admin/assets/{hash}", delete(admin_forget_asset))
}

// ---------------------------------------------------------------------------
// What a person may say about where a model came from
// ---------------------------------------------------------------------------

/// The licence an uploader states. A claim, not a finding: the server cannot
/// verify any of it and does not pretend to.
///
/// **Why the question is asked at all.** Using a model in a world you are
/// standing in and handing copies of it to everyone else are different acts under
/// almost every asset licence, and the second is what sharing to the library is.
/// The distinction is not hypothetical — Quaternius, whose characters this
/// project's avatars come from, moved off CC0 on 2026-08-28 to a licence that
/// permits the first and forbids the second in as many words.
///
/// So [`Licence::Other`] exists and is uploadable and usable and may never be
/// shared. That is the whole point of having four values rather than a boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Licence {
    /// The uploader made it.
    Own,
    /// Public domain equivalent: no attribution required, redistribution allowed.
    Cc0,
    /// Attribution required, redistribution allowed. The attribution line travels
    /// with the entry and is shown beside the model.
    CcBy,
    /// Anything else, including "I do not know". Usable by the person who
    /// uploaded it; never shareable.
    Other,
}

impl Licence {
    /// Whether a model under this licence may be handed to other people.
    ///
    /// The conservative direction is deliberate. Sharing under a licence that
    /// turns out to forbid it is a wrong we have done to the author and cannot
    /// take back once other people hold copies; refusing to share something that
    /// would have been fine costs one person one dialog.
    #[must_use]
    pub fn may_share(self) -> bool {
        matches!(self, Self::Own | Self::Cc0 | Self::CcBy)
    }

    /// Whether sharing needs an attribution line to go with it.
    #[must_use]
    pub fn needs_attribution(self) -> bool {
        matches!(self, Self::CcBy)
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::Cc0 => "cc0",
            Self::CcBy => "cc-by",
            Self::Other => "other",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "own" => Some(Self::Own),
            "cc0" => Some(Self::Cc0),
            "cc-by" => Some(Self::CcBy),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

/// How visible a shared model is.
///
/// A sequence rather than a hierarchy: `Shared` the moment somebody publishes it,
/// because a library that waits for review never fills; `Curated` once an
/// operator has given it a category and a tidy name in cartopy; `Hidden` when one
/// has decided it does not belong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Visibility {
    Shared,
    Curated,
    Hidden,
}

impl Visibility {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Curated => "curated",
            Self::Hidden => "hidden",
        }
    }

    /// Unknown text reads as `Hidden`.
    ///
    /// The safe direction for a value that arrives from a database somebody may
    /// have edited by hand: a typo makes one model invisible, where the other
    /// default would publish something an operator had taken down.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "shared" => Self::Shared,
            "curated" => Self::Curated,
            _ => Self::Hidden,
        }
    }
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// One line of somebody's inventory, as the picker draws it.
#[derive(Debug, Clone, Serialize)]
pub struct InventoryItem {
    pub id: i64,
    pub name: String,
    pub folder: String,
    pub asset: String,
    pub triangles: u64,
    pub byte_size: u64,
    pub licence: String,
    /// Whether this file is in the shared library, and how it appears there.
    /// `None` means it is private to whoever holds it.
    pub shared: Option<String>,
    pub created_at: String,
}

/// One entry in the shared library.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryEntry {
    pub asset: String,
    pub name: String,
    pub category: String,
    pub tags: Vec<String>,
    pub licence: String,
    /// The line a CC-BY model must be credited with, verbatim. Empty for the
    /// licences that need none.
    pub attribution: String,
    pub triangles: u64,
    pub byte_size: u64,
    pub visibility: String,
    pub shared_by: Option<String>,
}

/// An entry as a curator sees it: everything above, plus who uploaded the file
/// and what each of them said it was.
#[derive(Debug, Clone, Serialize)]
pub struct CuratorEntry {
    #[serde(flatten)]
    pub entry: LibraryEntry,
    pub uploaders: Vec<String>,
    pub in_world: i64,
    pub held_by: i64,
    pub created_at: String,
}

/// A stored file and what still refers to it.
///
/// `in_world` and `held_by` are the two independent references, and the reason
/// nothing here deletes a file automatically: a prim in the street holds the hash
/// directly, not the inventory item somebody rezzed it from, so an inventory that
/// has emptied says nothing about whether the model is still standing somewhere.
#[derive(Debug, Clone, Serialize)]
pub struct StoredAsset {
    pub hash: String,
    pub byte_size: u64,
    pub triangles: u64,
    pub nodes: u64,
    pub textures: u64,
    pub largest_texture: u32,
    pub in_world: i64,
    pub held_by: i64,
    pub shared: bool,
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// Upload
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct UploadQuery {
    /// What to call it in the uploader's inventory. Defaults to the licence-free
    /// truth, which is that nobody has named it yet.
    #[serde(default)]
    name: Option<String>,
    /// `own`, `cc0`, `cc-by` or `other`.
    licence: String,
    #[serde(default)]
    attribution: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    folder: Option<String>,
}

/// The longest name or folder accepted, in characters. Long enough for any real
/// label, short enough that a listing is a listing.
const MAX_LABEL_LEN: usize = 120;

/// `POST /api/assets` — store a model and put it in the caller's inventory.
///
/// The body is the `.glb` itself rather than a multipart form: there is exactly
/// one file and no other field that has to travel with it, and the metadata is
/// short enough to be a query string. That also keeps the size limit in one
/// place — the extractor's — rather than spread over a parser's parts.
///
/// **Uploading the same file twice is not an error.** The bytes are already
/// there, so the file is left alone and a second inventory item is made. Two
/// people finding the same bench and both wanting one is the ordinary case, and
/// so is one person doing it twice.
async fn upload_asset(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> ApiResult<Response> {
    let (principal, _username) = require_session(&state, &headers)?;
    let licence = Licence::parse(&q.licence)
        .ok_or_else(|| bad("licence must be one of own, cc0, cc-by, other"))?;
    if licence.needs_attribution() && q.attribution.as_deref().unwrap_or("").trim().is_empty() {
        return Err(bad(
            "a CC-BY model needs the credit line it must be shown with",
        ));
    }
    let name = check_label(q.name.as_deref().unwrap_or("Unnamed model"), "name")?;
    let folder = check_label(q.folder.as_deref().unwrap_or(""), "folder")?;

    // Parsing a model is CPU work measured in tens of milliseconds and this is an
    // async runtime shared with every connected client's frame loop. `body` is
    // already whole in memory, so the move is a pointer.
    let bytes = body.to_vec();
    let facts = tokio::task::spawn_blocking(move || {
        // A self-contained `.gltf` is repacked into the `.glb` everything
        // downstream expects; a `.glb` passes straight through and an external
        // reference is refused here rather than fetched (`assets::repack_gltf`).
        // What is inspected, hashed and stored is always the repacked bytes, so
        // there is exactly one kind of file in the store.
        let bytes = match assets::repack_gltf(&bytes)? {
            Some(repacked) => repacked,
            None => bytes,
        };
        assets::inspect(&bytes).map(|f| (f, bytes))
    })
    .await
    .map_err(|e| internal(anyhow::anyhow!("model inspection panicked: {e}")))?
    .map_err(bad)?;
    let (facts, bytes) = facts;

    // The file lands before the row does. The other order leaves a database that
    // promises a model the store does not have, which every later reader has to
    // second-guess; this order can at worst leave a file nothing points at, which
    // is what the orphan listing is for.
    assets::store(&state.config.asset_dir, &facts.hash, &bytes)
        .map_err(|e| internal(anyhow::anyhow!("could not store the model: {e}")))?;

    let item = with_conn(&state, |conn| {
        insert_asset(conn, &facts)?;
        claim_asset(
            conn,
            &facts.hash,
            principal,
            licence,
            q.attribution.as_deref(),
            q.source.as_deref(),
        )?;
        let id = insert_item(conn, principal, &facts.hash, &name, &folder)?;
        read_item(conn, principal, id)
    })?
    .ok_or_else(|| internal(anyhow::anyhow!("the item vanished as it was written")))?;

    tracing::info!(
        hash = %facts.hash,
        triangles = facts.triangles,
        bytes = facts.byte_size,
        "model uploaded"
    );
    Ok((StatusCode::CREATED, Json(item)).into_response())
}

/// `GET /api/assets/{hash}.glb` — the bytes.
///
/// Signed in, because everything else on this server is, and cached forever,
/// because the name is a hash of the content and so the answer can never change.
///
/// **A hidden model is refused here as well as in the listing.** Taking something
/// out of the catalogue while still serving it to every prim already wearing it
/// is the half-measure that looks like moderation and is not one: the objects in
/// the street are exactly what a report was about.
async fn download_asset(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<Response> {
    require_session(&state, &headers)?;
    let hash = name.strip_suffix(".glb").unwrap_or(&name);
    if !assets::is_hash(hash) {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such model".into()));
    }
    let hidden = with_conn(&state, |conn| {
        Ok(conn
            .query_row(
                "SELECT visibility FROM library_entries WHERE asset_hash = ?1",
                rusqlite::params![hash],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .is_some_and(|v| Visibility::parse(&v) == Visibility::Hidden))
    })?;
    if hidden {
        return Err(ApiError(
            StatusCode::GONE,
            "an operator has taken that model down".into(),
        ));
    }
    let Some(bytes) = assets::load(&state.config.asset_dir, hash) else {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such model".into()));
    };
    Ok((
        [
            ("content-type", "model/gltf-binary"),
            ("cache-control", "public, max-age=31536000, immutable"),
        ],
        bytes,
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// Inventory
// ---------------------------------------------------------------------------

/// `GET /api/inventory` — everything the caller holds.
async fn list_inventory(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<InventoryItem>>> {
    let (principal, _) = require_session(&state, &headers)?;
    let items = with_conn(&state, |conn| read_items(conn, principal))?;
    Ok(Json(items))
}

#[derive(Debug, Deserialize)]
pub struct ItemPatch {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    folder: Option<String>,
}

/// `PATCH /api/inventory/{id}` — rename an item or move it to another folder.
///
/// Absent leaves a field alone, matching the authoring surface's rule. Neither is
/// nullable, so the `present` deserialiser that surface needs is not needed here.
async fn update_item(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(patch): Json<ItemPatch>,
) -> ApiResult<Json<InventoryItem>> {
    let (principal, _) = require_session(&state, &headers)?;
    let name = patch
        .name
        .as_deref()
        .map(|n| check_label(n, "name"))
        .transpose()?;
    let folder = patch
        .folder
        .as_deref()
        .map(|f| check_label(f, "folder"))
        .transpose()?;
    let item = with_conn(&state, |conn| {
        if let Some(name) = &name {
            conn.execute(
                "UPDATE inventory_items SET name = ?3 WHERE id = ?1 AND account_id = ?2",
                rusqlite::params![id, principal.to_string(), name],
            )?;
        }
        if let Some(folder) = &folder {
            conn.execute(
                "UPDATE inventory_items SET folder = ?3 WHERE id = ?1 AND account_id = ?2",
                rusqlite::params![id, principal.to_string(), folder],
            )?;
        }
        read_item(conn, principal, id)
    })?;
    item.map(Json).ok_or_else(no_such_item)
}

/// `DELETE /api/inventory/{id}` — throw an item away.
///
/// The file stays. It may be in somebody else's inventory, it may be standing in
/// the street on a prim, and it may be in the shared library — none of which this
/// row knows about. `admin_list_assets` is where an operator sees what nothing
/// refers to any more.
async fn delete_item(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let (principal, _) = require_session(&state, &headers)?;
    let gone = with_conn(&state, |conn| {
        Ok(conn.execute(
            "DELETE FROM inventory_items WHERE id = ?1 AND account_id = ?2",
            rusqlite::params![id, principal.to_string()],
        )? > 0)
    })?;
    if gone {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(no_such_item())
    }
}

#[derive(Debug, Deserialize)]
pub struct GiveBody {
    to: String,
}

/// `POST /api/inventory/{id}/give` — put a copy of an item in someone else's
/// inventory.
///
/// A copy, not a move: the file is shared already and an inventory item is a
/// reference, so "giving" is one insert and the giver keeps theirs. That differs
/// from `/api/content/transfer`, which really does hand prims over, and the
/// difference is right — a prim is a thing in the world with one owner, an
/// inventory item is a note about a file.
///
/// Refused for a model the giver may not redistribute, for the reason
/// [`Licence::may_share`] gives.
async fn give_item(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<GiveBody>,
) -> ApiResult<Json<InventoryItem>> {
    let (principal, _) = require_session(&state, &headers)?;
    let item = with_conn_api(&state, |conn| {
        let Some(item) = read_item(conn, principal, id).map_err(internal)? else {
            return Ok(None);
        };
        let licence = Licence::parse(&item.licence).unwrap_or(Licence::Other);
        if !licence.may_share() {
            return Err(not_redistributable());
        }
        let Some((recipient, _)) = crate::auth::lookup_account(conn, &body.to).map_err(internal)?
        else {
            return Err(ApiError(
                StatusCode::NOT_FOUND,
                "no account by that name".into(),
            ));
        };
        if recipient == principal {
            return Err(bad("you already have that one"));
        }
        let new_id = insert_item(conn, recipient, &item.asset, &item.name, "").map_err(internal)?;
        read_item(conn, recipient, new_id).map_err(internal)
    })?;
    item.map(Json).ok_or_else(no_such_item)
}

// ---------------------------------------------------------------------------
// The shared library
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ShareBody {
    /// What it should be called in the catalogue. Defaults to the item's own name.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    tags: Option<String>,
}

/// `POST /api/inventory/{id}/share` — publish a model to everyone.
///
/// This is the act the licence question exists for. Using a model in a world is
/// not the same as handing copies of it to other people, and most asset licences
/// that are not CC0 draw the line in exactly that place — so a model claimed as
/// `other` is refused here and only here.
///
/// Sharing is per *file*, not per item: publishing your copy of a bench somebody
/// else already published changes nothing except who is recorded as having done
/// it first, which is why this is an upsert that leaves an existing entry alone.
async fn share_item(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<ShareBody>,
) -> ApiResult<Json<LibraryEntry>> {
    let (principal, username) = require_session(&state, &headers)?;
    let category = body.category.unwrap_or_else(|| "other".into());
    let tags = body.tags.unwrap_or_default();
    let entry = with_conn_api(&state, |conn| {
        let Some(item) = read_item(conn, principal, id).map_err(internal)? else {
            return Ok(None);
        };
        let licence = Licence::parse(&item.licence).unwrap_or(Licence::Other);
        if !licence.may_share() {
            return Err(not_redistributable());
        }
        let attribution: Option<String> = conn
            .query_row(
                "SELECT attribution FROM asset_claims WHERE asset_hash = ?1 AND account_id = ?2",
                rusqlite::params![item.asset, principal.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| internal(e.into()))?
            .flatten();
        let name = body.name.clone().unwrap_or_else(|| item.name.clone());
        // An entry that is already there is left as it is — including its
        // category and its curated name, which somebody may have spent time on.
        // The one thing that must still happen is that a *hidden* entry does not
        // come back to life because a second person shared the same file.
        conn.execute(
            "INSERT INTO library_entries
                 (asset_hash, name, category, tags, licence, attribution, shared_by, visibility)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'shared')
             ON CONFLICT (asset_hash) DO NOTHING",
            rusqlite::params![
                item.asset,
                name,
                category,
                tags,
                licence.as_str(),
                attribution,
                username,
            ],
        )
        .map_err(|e| internal(e.into()))?;
        read_entry(conn, &item.asset).map_err(internal)
    })?;
    entry.map(Json).ok_or_else(no_such_item)
}

/// `DELETE /api/inventory/{id}/share` — take a model back out of the library.
///
/// Only the person who shared it, or an admin. Anyone else holding a copy of the
/// same file has no say: they did not publish it.
async fn unshare_item(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let (principal, username) = require_session(&state, &headers)?;
    let admin = is_admin(&state, principal)?;
    let gone = with_conn(&state, |conn| {
        let Some(item) = read_item(conn, principal, id)? else {
            return Ok(false);
        };
        let n = if admin {
            conn.execute(
                "DELETE FROM library_entries WHERE asset_hash = ?1",
                rusqlite::params![item.asset],
            )?
        } else {
            conn.execute(
                "DELETE FROM library_entries WHERE asset_hash = ?1 AND shared_by = ?2",
                rusqlite::params![item.asset, username],
            )?
        };
        Ok(n > 0)
    })?;
    if gone {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError(
            StatusCode::FORBIDDEN,
            "that model was shared by somebody else".into(),
        ))
    }
}

#[derive(Debug, Deserialize)]
pub struct BrowseQuery {
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    q: Option<String>,
}

/// `GET /api/library` — the catalogue, for the in-world picker.
///
/// Hidden entries are absent. Uncurated ones are present, because a library that
/// only shows what an operator has been round to is empty on the day it opens.
// ---------------------------------------------------------------------------
// What the street's own objects look like here
// ---------------------------------------------------------------------------

/// One form of street object, and the model this server draws instead of it.
///
/// `V18__placeholder_models.sql` has the argument for why this is world-wide and
/// not per person. In short: nobody placed a lamppost, so nobody owns the
/// decision about it; two people on one street seeing different lampposts is a
/// disagreement about what is there rather than a preference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceholderModel {
    /// A slug from `cartopolis_core::placeholders`.
    pub kind: String,
    /// The asset hash to draw. Fetched over `/api/assets/{hash}.glb` like any
    /// other model — a placeholder is not a new kind of file.
    pub asset: String,
    pub scale: f32,
    /// Degrees, added to the angle the client computed for the object.
    pub yaw_offset: f32,
    /// What it replaces, for an operator reading the list back.
    pub set_by: String,
}

/// What an operator sends to set one.
#[derive(Debug, Deserialize)]
struct PlaceholderBody {
    asset: String,
    scale: Option<f32>,
    yaw_offset: Option<f32>,
}

/// The whole table, for any signed-in client.
///
/// Not admin-only, and not optional either: this is what the client has to read
/// before it draws a street, so it sits beside `/api/library` on the surface a
/// player already reaches. There is nothing in it a player could not see by
/// walking outside and looking at a lamppost.
async fn list_placeholders(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<PlaceholderModel>>> {
    require_session(&state, &headers)?;
    let rows = with_conn(&state, |conn| {
        let mut stmt = conn.prepare(
            "SELECT kind, asset_hash, scale, yaw_offset, set_by
               FROM placeholder_models
              ORDER BY kind",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(PlaceholderModel {
                    kind: row.get(0)?,
                    asset: row.get(1)?,
                    scale: row.get(2)?,
                    yaw_offset: row.get(3)?,
                    set_by: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    Ok(Json(rows))
}

/// Replace a form with a model, or change how the replacement is placed.
///
/// Three refusals, and each is a thing that would otherwise fail silently on
/// every client at once rather than here: a slug this build does not know, a hash
/// no file was ever stored under, and a scale outside what could be a street
/// object. The client cannot report any of them — it draws a city from a register
/// and has nowhere to say "that lamppost did not load".
async fn set_placeholder(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(kind): Path<String>,
    Json(body): Json<PlaceholderBody>,
) -> ApiResult<Json<PlaceholderModel>> {
    let (_principal, username) = require_admin(&state, &headers)?;
    if !cartopolis_core::placeholders::is_placeholder_kind(&kind) {
        return Err(bad(format!(
            "{kind} is not a street object this server draws — one of: {}",
            cartopolis_core::placeholders::PLACEHOLDER_KINDS.join(", ")
        )));
    }
    if !assets::is_hash(&body.asset) {
        return Err(bad("that is not an asset hash"));
    }
    let scale = body.scale.unwrap_or(1.0);
    if !scale.is_finite()
        || scale < cartopolis_core::placeholders::MIN_PLACEHOLDER_SCALE
        || scale > cartopolis_core::placeholders::MAX_PLACEHOLDER_SCALE
    {
        return Err(bad(format!(
            "a scale of {scale} is outside {}–{}",
            cartopolis_core::placeholders::MIN_PLACEHOLDER_SCALE,
            cartopolis_core::placeholders::MAX_PLACEHOLDER_SCALE
        )));
    }
    let yaw_offset = body.yaw_offset.unwrap_or(0.0);
    if !yaw_offset.is_finite() {
        return Err(bad("that is not an angle"));
    }

    with_conn_api(&state, |conn| {
        let known: bool = conn
            .query_row(
                "SELECT 1 FROM assets WHERE hash = ?1",
                rusqlite::params![body.asset],
                |_| Ok(true),
            )
            .optional()
            .map_err(|e| internal(e.into()))?
            .unwrap_or(false);
        if !known {
            return Err(ApiError(
                StatusCode::NOT_FOUND,
                "no model is stored under that hash".into(),
            ));
        }
        conn.execute(
            "INSERT INTO placeholder_models (kind, asset_hash, scale, yaw_offset, set_by)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (kind) DO UPDATE SET
                 asset_hash = excluded.asset_hash,
                 scale      = excluded.scale,
                 yaw_offset = excluded.yaw_offset,
                 set_by     = excluded.set_by,
                 set_at     = datetime('now')",
            rusqlite::params![kind, body.asset, scale, yaw_offset, username],
        )
        .map_err(|e| internal(e.into()))?;
        Ok(())
    })?;

    tracing::info!(%kind, asset = %body.asset, scale, "placeholder model set");
    Ok(Json(PlaceholderModel {
        kind,
        asset: body.asset,
        scale,
        yaw_offset,
        set_by: username,
    }))
}

/// Put a form back to the shape built into the client.
///
/// Deleting an unset one is not an error: "there is no override for a bin" is the
/// state the caller asked for, and answering 404 would make a tidy-up script fail
/// on a server that was already tidy.
async fn clear_placeholder(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(kind): Path<String>,
) -> ApiResult<StatusCode> {
    require_admin(&state, &headers)?;
    with_conn(&state, |conn| {
        conn.execute(
            "DELETE FROM placeholder_models WHERE kind = ?1",
            rusqlite::params![kind],
        )?;
        Ok(())
    })?;
    tracing::info!(%kind, "placeholder model cleared");
    Ok(StatusCode::NO_CONTENT)
}

async fn browse_library(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Query(q): Query<BrowseQuery>,
) -> ApiResult<Json<Vec<LibraryEntry>>> {
    require_session(&state, &headers)?;
    let entries = with_conn(&state, |conn| read_entries(conn, false))?;
    let needle = q.q.map(|s| s.to_lowercase());
    let entries = entries
        .into_iter()
        .filter(|e| q.category.as_ref().is_none_or(|c| &e.category == c))
        .filter(|e| {
            needle.as_ref().is_none_or(|n| {
                e.name.to_lowercase().contains(n)
                    || e.tags.iter().any(|t| t.to_lowercase().contains(n))
            })
        })
        .collect();
    Ok(Json(entries))
}

/// `POST /api/library/{hash}/take` — copy a library entry into your inventory.
///
/// The model can be placed without this — the picker can build a prim straight
/// from a catalogue entry — so "take" is about keeping one: an item you have
/// named, filed and can find again after the library has moved on.
async fn take_from_library(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(hash): Path<String>,
) -> ApiResult<Response> {
    let (principal, _) = require_session(&state, &headers)?;
    if !assets::is_hash(&hash) {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such model".into()));
    }
    let item = with_conn(&state, |conn| {
        let Some(entry) = read_entry(conn, &hash)? else {
            return Ok(None);
        };
        if Visibility::parse(&entry.visibility) == Visibility::Hidden {
            return Ok(None);
        }
        let id = insert_item(conn, principal, &hash, &entry.name, "Library")?;
        read_item(conn, principal, id)
    })?;
    let item = item.ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "no such model".into()))?;
    Ok((StatusCode::CREATED, Json(item)).into_response())
}

// ---------------------------------------------------------------------------
// Curation, from cartopy
// ---------------------------------------------------------------------------

/// `GET /api/admin/library` — every entry including the hidden ones, with who
/// uploaded each file and how many copies of it are standing in the world.
async fn admin_list_library(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<CuratorEntry>>> {
    require_admin(&state, &headers)?;
    let entries = with_conn(&state, |conn| {
        let entries = read_entries(conn, true)?;
        entries
            .into_iter()
            .map(|entry| {
                let uploaders = uploaders_of(conn, &entry.asset)?;
                let (in_world, held_by) = reference_counts(conn, &entry.asset)?;
                let created_at = conn.query_row(
                    "SELECT created_at FROM library_entries WHERE asset_hash = ?1",
                    rusqlite::params![entry.asset],
                    |row| row.get(0),
                )?;
                Ok(CuratorEntry {
                    entry,
                    uploaders,
                    in_world,
                    held_by,
                    created_at,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()
    })?;
    Ok(Json(entries))
}

#[derive(Debug, Deserialize)]
pub struct CuratePatch {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    tags: Option<String>,
    #[serde(default)]
    visibility: Option<Visibility>,
    /// The credit line. Curators fix these, because an uploader typing one by
    /// hand gets it wrong and a wrong credit is worse than a missing model.
    #[serde(default)]
    attribution: Option<String>,
}

/// `PATCH /api/admin/library/{hash}` — the curator's edit.
///
/// Setting `visibility` to `curated` stamps `curated_at`, which is how "an
/// operator has looked at this" is told apart from "nobody has yet". Setting it
/// to `hidden` takes the model out of the catalogue *and* stops the download
/// route serving it, so every prim already wearing it falls back to the
/// placeholder.
async fn curate_entry(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(hash): Path<String>,
    Json(patch): Json<CuratePatch>,
) -> ApiResult<Json<LibraryEntry>> {
    require_admin(&state, &headers)?;
    if !assets::is_hash(&hash) {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such model".into()));
    }
    let entry = with_conn(&state, |conn| {
        if let Some(name) = &patch.name {
            conn.execute(
                "UPDATE library_entries SET name = ?2 WHERE asset_hash = ?1",
                rusqlite::params![hash, name],
            )?;
        }
        if let Some(category) = &patch.category {
            conn.execute(
                "UPDATE library_entries SET category = ?2 WHERE asset_hash = ?1",
                rusqlite::params![hash, category],
            )?;
        }
        if let Some(tags) = &patch.tags {
            conn.execute(
                "UPDATE library_entries SET tags = ?2 WHERE asset_hash = ?1",
                rusqlite::params![hash, tags],
            )?;
        }
        if let Some(attribution) = &patch.attribution {
            conn.execute(
                "UPDATE library_entries SET attribution = ?2 WHERE asset_hash = ?1",
                rusqlite::params![hash, attribution],
            )?;
        }
        if let Some(visibility) = patch.visibility {
            conn.execute(
                "UPDATE library_entries
                    SET visibility = ?2,
                        curated_at = CASE WHEN ?2 = 'curated' THEN datetime('now') ELSE curated_at END
                  WHERE asset_hash = ?1",
                rusqlite::params![hash, visibility.as_str()],
            )?;
        }
        read_entry(conn, &hash)
    })?;
    entry
        .map(Json)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "no such model".into()))
}

/// `GET /api/admin/assets` — every stored file, with what refers to it.
///
/// This is the orphan report. Nothing here deletes anything: a file with no
/// references today may be one somebody is about to rez from an export, and a
/// wrongly collected model is a hole in a build that cannot be repaired from the
/// database.
async fn admin_list_assets(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<StoredAsset>>> {
    require_admin(&state, &headers)?;
    let rows = with_conn(&state, |conn| {
        let mut stmt = conn.prepare(
            "SELECT a.hash, a.byte_size, a.triangles, a.nodes, a.textures, a.largest_texture,
                    a.created_at,
                    (SELECT COUNT(*) FROM prims p WHERE p.model = a.hash),
                    (SELECT COUNT(*) FROM inventory_items i WHERE i.asset_hash = a.hash),
                    EXISTS (SELECT 1 FROM library_entries l WHERE l.asset_hash = a.hash)
               FROM assets a
              ORDER BY a.created_at DESC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(StoredAsset {
                    hash: row.get(0)?,
                    byte_size: row.get(1)?,
                    triangles: row.get(2)?,
                    nodes: row.get(3)?,
                    textures: row.get(4)?,
                    largest_texture: row.get(5)?,
                    created_at: row.get(6)?,
                    in_world: row.get(7)?,
                    held_by: row.get(8)?,
                    shared: row.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })?;
    Ok(Json(rows))
}

/// `DELETE /api/admin/assets/{hash}` — remove a file that nothing refers to.
///
/// Refused while anything still points at it, and the refusal says what does.
/// Deleting a file out from under a prim would leave an object in the street that
/// can never be drawn again by anyone, and there is no undo for that short of the
/// uploader still having the file.
async fn admin_forget_asset(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(hash): Path<String>,
) -> ApiResult<StatusCode> {
    require_admin(&state, &headers)?;
    if !assets::is_hash(&hash) {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such model".into()));
    }
    let (in_world, held_by) = with_conn(&state, |conn| reference_counts(conn, &hash))?;
    // A placeholder is the third way a file is referred to, and the one nobody
    // would think to check: it is on no prim and in nobody's inventory, and
    // removing it would leave every lamppost in the city pointing at a hash the
    // store no longer has.
    let standing_in: i64 = with_conn(&state, |conn| {
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM placeholder_models WHERE asset_hash = ?1",
            rusqlite::params![hash],
            |row| row.get(0),
        )?)
    })?;
    if in_world > 0 || held_by > 0 || standing_in > 0 {
        return Err(ApiError(
            StatusCode::CONFLICT,
            format!(
                "that model is on {in_world} prim(s), in {held_by} inventory(ies) and stands in for \
                 {standing_in} kind(s) of street object; it cannot be removed while anything refers to it"
            ),
        ));
    }
    with_conn(&state, |conn| {
        conn.execute(
            "DELETE FROM library_entries WHERE asset_hash = ?1",
            rusqlite::params![hash],
        )?;
        conn.execute(
            "DELETE FROM asset_claims WHERE asset_hash = ?1",
            rusqlite::params![hash],
        )?;
        conn.execute(
            "DELETE FROM assets WHERE hash = ?1",
            rusqlite::params![hash],
        )?;
        Ok(())
    })?;
    let path = std::path::Path::new(&state.config.asset_dir).join(format!("{hash}.glb"));
    if let Err(e) = std::fs::remove_file(&path) {
        // The rows are gone and that is the part that matters. A file left behind
        // is invisible to everything and shows up in the next orphan listing.
        tracing::warn!(%hash, "removed the model's rows but not its file: {e}");
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Rows, in one place
// ---------------------------------------------------------------------------

/// Record a file. Idempotent: the same bytes measure the same both times.
pub fn insert_asset(conn: &Connection, facts: &assets::AssetFacts) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO assets (hash, byte_size, triangles, nodes, textures, largest_texture)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (hash) DO NOTHING",
        rusqlite::params![
            facts.hash,
            facts.byte_size,
            facts.triangles,
            facts.nodes,
            facts.textures,
            facts.largest_texture,
        ],
    )?;
    Ok(())
}

/// Record what an uploader said about where a file came from.
///
/// One row per (file, uploader). A second upload by the same person replaces
/// their claim — they may have got the licence wrong the first time — and never
/// touches anybody else's.
pub fn claim_asset(
    conn: &Connection,
    hash: &str,
    account: Uuid,
    licence: Licence,
    attribution: Option<&str>,
    source: Option<&str>,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO asset_claims (asset_hash, account_id, licence, attribution, source_note)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (asset_hash, account_id)
         DO UPDATE SET licence = excluded.licence,
                       attribution = excluded.attribution,
                       source_note = excluded.source_note",
        rusqlite::params![
            hash,
            account.to_string(),
            licence.as_str(),
            attribution,
            source
        ],
    )?;
    Ok(())
}

fn insert_item(
    conn: &Connection,
    account: Uuid,
    hash: &str,
    name: &str,
    folder: &str,
) -> anyhow::Result<i64> {
    conn.execute(
        "INSERT INTO inventory_items (account_id, asset_hash, name, folder)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![account.to_string(), hash, name, folder],
    )?;
    Ok(conn.last_insert_rowid())
}

/// The columns behind an [`InventoryItem`], joined to the file's measurements and
/// to whether it is in the library.
const ITEM_SELECT: &str = "SELECT i.id, i.name, i.folder, i.asset_hash, i.created_at,
            a.triangles, a.byte_size,
            COALESCE(c.licence, 'other'),
            l.visibility
       FROM inventory_items i
       JOIN assets a ON a.hash = i.asset_hash
       LEFT JOIN asset_claims c
              ON c.asset_hash = i.asset_hash AND c.account_id = i.account_id
       LEFT JOIN library_entries l ON l.asset_hash = i.asset_hash";

fn item_from_row(row: &rusqlite::Row) -> rusqlite::Result<InventoryItem> {
    Ok(InventoryItem {
        id: row.get(0)?,
        name: row.get(1)?,
        folder: row.get(2)?,
        asset: row.get(3)?,
        created_at: row.get(4)?,
        triangles: row.get(5)?,
        byte_size: row.get(6)?,
        licence: row.get(7)?,
        shared: row.get(8)?,
    })
}

fn read_item(conn: &Connection, account: Uuid, id: i64) -> anyhow::Result<Option<InventoryItem>> {
    let sql = format!("{ITEM_SELECT} WHERE i.id = ?1 AND i.account_id = ?2");
    Ok(conn
        .query_row(
            &sql,
            rusqlite::params![id, account.to_string()],
            item_from_row,
        )
        .optional()?)
}

fn read_items(conn: &Connection, account: Uuid) -> anyhow::Result<Vec<InventoryItem>> {
    let sql = format!("{ITEM_SELECT} WHERE i.account_id = ?1 ORDER BY i.folder, i.name");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params![account.to_string()], item_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

const ENTRY_SELECT: &str = "SELECT l.asset_hash, l.name, l.category, l.tags, l.licence,
            COALESCE(l.attribution, ''), l.visibility, l.shared_by,
            a.triangles, a.byte_size
       FROM library_entries l
       JOIN assets a ON a.hash = l.asset_hash";

fn entry_from_row(row: &rusqlite::Row) -> rusqlite::Result<LibraryEntry> {
    let tags: String = row.get(3)?;
    Ok(LibraryEntry {
        asset: row.get(0)?,
        name: row.get(1)?,
        category: row.get(2)?,
        tags: tags
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect(),
        licence: row.get(4)?,
        attribution: row.get(5)?,
        visibility: row.get(6)?,
        shared_by: row.get(7)?,
        triangles: row.get(8)?,
        byte_size: row.get(9)?,
    })
}

fn read_entry(conn: &Connection, hash: &str) -> anyhow::Result<Option<LibraryEntry>> {
    let sql = format!("{ENTRY_SELECT} WHERE l.asset_hash = ?1");
    Ok(conn
        .query_row(&sql, rusqlite::params![hash], entry_from_row)
        .optional()?)
}

fn read_entries(conn: &Connection, include_hidden: bool) -> anyhow::Result<Vec<LibraryEntry>> {
    let sql = if include_hidden {
        format!("{ENTRY_SELECT} ORDER BY l.category, l.name")
    } else {
        format!("{ENTRY_SELECT} WHERE l.visibility != 'hidden' ORDER BY l.category, l.name")
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], entry_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn uploaders_of(conn: &Connection, hash: &str) -> anyhow::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(ac.username, c.account_id)
           FROM asset_claims c
           LEFT JOIN accounts ac ON ac.id = c.account_id
          WHERE c.asset_hash = ?1
          ORDER BY c.created_at",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![hash], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// How many prims wear this file, and how many inventories hold it.
///
/// Two counts and not a sum, because they answer different questions: the first
/// is "would deleting this leave an object nobody can draw?", the second is
/// "would it take something out of somebody's inventory?".
fn reference_counts(conn: &Connection, hash: &str) -> anyhow::Result<(i64, i64)> {
    let in_world: i64 = conn.query_row(
        "SELECT COUNT(*) FROM prims WHERE model = ?1",
        rusqlite::params![hash],
        |row| row.get(0),
    )?;
    let held_by: i64 = conn.query_row(
        "SELECT COUNT(*) FROM inventory_items WHERE asset_hash = ?1",
        rusqlite::params![hash],
        |row| row.get(0),
    )?;
    Ok((in_world, held_by))
}

/// Whether a model may be drawn: it is stored and no operator has hidden it.
///
/// `net.rs` asks this before letting a prim wear one, so a hash that is a typo, a
/// guess or a model from another server is refused at the point it is set rather
/// than becoming a permanently grey prim in the street.
pub fn model_is_available(conn: &Connection, hash: &str) -> anyhow::Result<bool> {
    if !assets::is_hash(hash) {
        return Ok(false);
    }
    let stored: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM assets WHERE hash = ?1)",
        rusqlite::params![hash],
        |row| row.get(0),
    )?;
    if !stored {
        return Ok(false);
    }
    let hidden: bool = conn
        .query_row(
            "SELECT visibility FROM library_entries WHERE asset_hash = ?1",
            rusqlite::params![hash],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .is_some_and(|v| Visibility::parse(&v) == Visibility::Hidden);
    Ok(!hidden)
}

// ---------------------------------------------------------------------------
// Plumbing
// ---------------------------------------------------------------------------

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

fn no_such_item() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "no such item".into())
}

fn check_label(s: &str, what: &str) -> ApiResult<String> {
    let trimmed = s.trim();
    if trimmed.chars().count() > MAX_LABEL_LEN {
        return Err(bad(format!(
            "that {what} is longer than {MAX_LABEL_LEN} characters"
        )));
    }
    Ok(trimmed.to_string())
}

/// Run a closure under the database lock.
///
/// The lock is held for the body and no longer. Every caller here is a short
/// read or a couple of inserts; nothing in this module does network work or model
/// parsing while holding it, because the tick loop takes the same lock.
fn with_conn<T>(
    state: &HttpState,
    f: impl FnOnce(&Connection) -> anyhow::Result<T>,
) -> ApiResult<T> {
    let guard = state.conn.lock().map_err(|_| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database unavailable".into(),
        )
    })?;
    f(&guard).map_err(internal)
}

/// The refusal both sharing and giving away come down to.
///
/// One sentence in one place, because it is the same rule twice: publishing a
/// model and handing somebody a copy are the same act as far as an asset licence
/// is concerned, and a person who meets it from two directions should not get two
/// different explanations.
fn not_redistributable() -> ApiError {
    ApiError(
        StatusCode::FORBIDDEN,
        "you said this model came from somewhere else, so it cannot be handed on. \
         Only your own work, CC0 and CC BY can be shared."
            .into(),
    )
}

/// [`with_conn`] for a body that can itself refuse.
///
/// The plain one maps every error through `http::internal`, which throws the
/// message away and answers 500 — right for a database fault, wrong for "that
/// licence does not permit this", which is a decision this module made and has to
/// be able to explain.
fn with_conn_api<T>(
    state: &HttpState,
    f: impl FnOnce(&Connection) -> ApiResult<T>,
) -> ApiResult<T> {
    let guard = state.conn.lock().map_err(|_| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database unavailable".into(),
        )
    })?;
    f(&guard)
}

fn is_admin(state: &HttpState, principal: Uuid) -> ApiResult<bool> {
    with_conn(state, |conn| Ok(crate::auth::is_admin(conn, principal)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sharing is the act the licence question exists for, and the conservative
    /// direction is the one that matters: a model somebody may *use* is not
    /// necessarily one they may hand out.
    #[test]
    fn only_a_redistributable_licence_may_be_shared() {
        assert!(Licence::Own.may_share());
        assert!(Licence::Cc0.may_share());
        assert!(Licence::CcBy.may_share());
        assert!(!Licence::Other.may_share());
    }

    /// Only CC-BY needs the credit line, and it needs it at upload — the moment
    /// the person still has the answer in front of them.
    #[test]
    fn attribution_is_required_where_the_licence_requires_it() {
        assert!(Licence::CcBy.needs_attribution());
        assert!(!Licence::Cc0.needs_attribution());
        assert!(!Licence::Own.needs_attribution());
    }

    #[test]
    fn licence_names_round_trip() {
        for l in [Licence::Own, Licence::Cc0, Licence::CcBy, Licence::Other] {
            assert_eq!(Licence::parse(l.as_str()), Some(l));
        }
        assert_eq!(Licence::parse("cc-by-nc"), None);
    }

    /// A visibility that does not parse reads as hidden. The other default would
    /// republish something an operator had taken down.
    #[test]
    fn an_unreadable_visibility_hides() {
        assert_eq!(Visibility::parse("shared"), Visibility::Shared);
        assert_eq!(Visibility::parse("curated"), Visibility::Curated);
        assert_eq!(Visibility::parse("hidden"), Visibility::Hidden);
        assert_eq!(Visibility::parse("Shared"), Visibility::Hidden);
        assert_eq!(Visibility::parse(""), Visibility::Hidden);
    }
}
