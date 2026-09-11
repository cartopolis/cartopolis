//! The authoring API: build and script the world from outside a viewer.
//!
//! # Why this exists at all, when there is an editor
//!
//! The in-world tools — the create dialog, the gizmo, the script editor — are the
//! right way to make one object while looking at it, and they are unusable for
//! everything else: they need a person, a GPU and a signed-in session, so a world
//! cannot be built by a script, restored from a file, reviewed in a diff, or set
//! up by an agent with no screen. Every one of those is an ordinary thing to want
//! and none of them was possible.
//!
//! So this is the same set of operations — make a prim, change it, give it a
//! script, delete it — reachable over HTTP with an admin token.
//!
//! # Why HTTP and not the wire
//!
//! Exactly the reason account management is HTTP (`crate::http`'s module note):
//! these are *operator* actions. Putting them on `NetMessage` would route an
//! authorisation decision through the same path as a movement intent, and would
//! move every client's protocol version whenever the authoring surface changed.
//! The wire keeps `CreatePrim`/`UpdatePrim`, which are a *player* building
//! something in front of them and are checked against the world's budget; this
//! is an operator building something they may not be looking at.
//!
//! # What it shares with the wire, and what that costs
//!
//! It goes through [`SimWorld`] rather than SQLite, which is not an
//! implementation detail: the world is the authority, it holds prims in memory,
//! and a row written behind its back would be invisible until a restart. That
//! means every handler here takes the world's write lock, so each one does its
//! work and drops it — the tick loop is waiting on the same lock 20 times a
//! second.
//!
//! Changes are announced the same way a script's are: a `PrimUpsert` (or a
//! `PrimRemoved`) on the snapshot broadcast, so a client that is already
//! connected sees the prim appear without waiting for anything to re-send it.
//!
//! # Degrees, not radians
//!
//! Rotations here are **degrees**, like the edit dialog's fields and like
//! `vfSetRot`, and are converted at this boundary. The wire and the client's
//! `Quat::from_euler` are radians, and the rule this tree follows is that only
//! one of the two conventions is ever in flight past the API boundary — the
//! author-facing side is the one that gets degrees.

use crate::http::{require_admin, ApiError, ApiResult, HttpState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use cartopolis_core::{
    encode_app_frame, GeoAnchor, NetMessage, ParticleEffect, PrimDto, PrimGeometry, PrimSurface,
};
use glam::Vec3;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;

pub fn routes() -> Router<HttpState> {
    Router::new()
        // Owner-scoped, and therefore *not* under `/api/admin`: exporting your own
        // build is a thing any signed-in account may do, and an admin token is
        // neither required nor sufficient (see [`export_content`]).
        .route("/api/content/export", get(export_content))
        .route("/api/content/import", post(import_content))
        .route("/api/content/transfer", post(transfer_content))
        .route("/api/admin/capacity", get(get_capacity))
        .route("/api/admin/prims", get(list_prims).post(create_prim))
        .route(
            "/api/admin/prims/{id}",
            get(get_prim).patch(update_prim).delete(delete_prim),
        )
        .route("/api/admin/prims/{id}/script", put(put_script))
        .route("/api/admin/content", get(list_content))
        .route("/api/admin/content/{name}", post(install_content))
}

// ---------------------------------------------------------------------------
// The request body
// ---------------------------------------------------------------------------

/// Serde's answer to "was this field absent, or present and null?".
///
/// `Option<Option<T>>` alone cannot tell them apart — the outer `Option` swallows
/// the null — and the difference is the whole of "leave the texture alone" versus
/// "take the texture off". Without it, clearing a field would need a second
/// boolean beside every nullable one.
fn present<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::<T>::deserialize(d)?))
}

/// A particle emitter, as an author writes one: every field optional, the missing
/// ones taken from a preset.
///
/// **Not `ParticleEffect` itself**, and the reason is the wire. That type is
/// postcard-encoded in every `WorldSnapshot`, and postcard is not
/// self-describing: it reads fields positionally, so giving them serde defaults
/// or `Option`s would change what the bytes mean and desync every client. The
/// leniency belongs at the boundary where a person is typing, which is here.
///
/// It also makes the two authoring surfaces behave the same. `vfParticles{ color
/// = {0,0.4,1} }` is a blue flame with everything else left alone, and so is
/// `{"color_start": [0,0.4,1]}` — before this, the second was an error listing
/// the thirteen fields it had not mentioned.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParticleSpec {
    /// Which preset the unmentioned fields come from — a one-shot blast, or a
    /// continuous flame. Exactly the choice `vfParticleBurst` versus
    /// `vfParticles` makes on the script side.
    #[serde(default)]
    pub burst: bool,
    pub count: Option<u16>,
    pub life: Option<f32>,
    pub speed: Option<f32>,
    pub spread_deg: Option<f32>,
    pub gravity: Option<f32>,
    pub source_radius: Option<f32>,
    pub size_start: Option<f32>,
    pub size_end: Option<f32>,
    pub color_start: Option<[f32; 3]>,
    pub color_end: Option<[f32; 3]>,
    pub alpha: Option<f32>,
    pub turbulence: Option<f32>,
    /// Almost never worth writing by hand — see [`ParticleSpec::resolve`].
    pub sequence: Option<u32>,
}

impl ParticleSpec {
    /// The effect this describes, given whatever the prim already carried.
    ///
    /// `previous` is what decides the sequence when the author did not give one,
    /// and getting that wrong is the difference between an explosion and nothing
    /// at all: a client replays a burst once per `(prim, sequence)`, so re-POSTing
    /// an identical blast with the same number is a request the client correctly
    /// ignores. Authoring a burst therefore *bumps* it by default, and a
    /// continuous emitter — which is not replayed and has nothing to re-arm —
    /// keeps whatever was there.
    #[must_use]
    pub fn resolve(&self, previous: Option<&ParticleEffect>) -> ParticleEffect {
        let base = if self.burst {
            ParticleEffect::blast()
        } else {
            ParticleEffect::fire()
        };
        let last = previous.map_or(0, |p| p.sequence);
        ParticleEffect {
            count: self.count.unwrap_or(base.count),
            life: self.life.unwrap_or(base.life),
            speed: self.speed.unwrap_or(base.speed),
            spread_deg: self.spread_deg.unwrap_or(base.spread_deg),
            gravity: self.gravity.unwrap_or(base.gravity),
            source_radius: self.source_radius.unwrap_or(base.source_radius),
            size_start: self.size_start.unwrap_or(base.size_start),
            size_end: self.size_end.unwrap_or(base.size_end),
            color_start: self.color_start.unwrap_or(base.color_start),
            color_end: self.color_end.unwrap_or(base.color_end),
            alpha: self.alpha.unwrap_or(base.alpha),
            turbulence: self.turbulence.unwrap_or(base.turbulence),
            burst: self.burst,
            sequence: self.sequence.unwrap_or(if self.burst {
                last.wrapping_add(1)
            } else {
                last.max(1)
            }),
        }
        .sanitized()
    }
}

/// A prim, as an author describes one. Every field is optional: on a create the
/// missing ones take a documented default, and on a patch they mean "leave it".
///
/// It is deliberately **not** `PrimDto`. A DTO is the full authoritative state
/// including the server-assigned id and the client-frame position echo, so
/// requiring one would make the simplest possible request — "put a red cube
/// here" — a twenty-field document, and would let a caller write fields that are
/// not theirs to write.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrimSpec {
    pub name: Option<String>,
    /// `box`, `sphere`, `cylinder`, `cone` or `torus`.
    pub shape: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// Metres above the ground. This is the durable
    /// half of the vertical placement, and the only part of `position` an author
    /// sets: horizontal placement is `latitude`/`longitude` (ADR-029).
    pub altitude: Option<f32>,
    /// Euler XYZ in **degrees**, as the edit dialog shows them.
    pub rotation: Option<[f32; 3]>,
    pub scale: Option<[f32; 3]>,
    pub color: Option<[f32; 3]>,
    #[serde(default, deserialize_with = "present")]
    pub texture_id: Option<Option<String>>,
    /// A stored model asset's hash, or an explicit `null` to take it off and go
    /// back to the procedural shape.
    ///
    /// Unlike the wire, this is **not** checked against the store. An author is an
    /// operator with a token, the check exists to stop a builder mistyping a hash
    /// into an undrawable prim, and an authoring script that seeds a world before
    /// uploading its models to it is a legitimate order to do things in.
    #[serde(default, deserialize_with = "present")]
    pub model: Option<Option<String>>,
    pub surface: Option<PrimSurface>,
    pub geometry: Option<PrimGeometry>,
    /// A particle emitter; `null` takes an existing one off.
    #[serde(default, deserialize_with = "present")]
    pub particles: Option<Option<ParticleSpec>>,
    /// Luau source. Empty string removes the script, exactly as the editor's
    /// empty buffer does. Absent leaves whatever is there.
    pub script: Option<String>,
}

impl PrimSpec {
    /// A live prim, described the way an author would have written it.
    ///
    /// The inverse of [`apply`], and deliberately lossy in one direction: the id
    /// and the client-frame `position.x/z` echo are **not** carried. An id from
    /// one server means nothing on another, and x/z are re-derived from the geo
    /// anchor by every client anyway (ADR-029). What is left is the part that is
    /// actually about the object.
    #[must_use]
    pub fn of(p: &PrimDto, script: Option<String>) -> Self {
        Self {
            name: Some(p.name.clone()),
            shape: Some(p.shape.clone()),
            latitude: Some(p.geo.latitude),
            longitude: Some(p.geo.longitude),
            altitude: Some(p.position.y),
            // Degrees on this surface, radians on the wire — the same conversion
            // `apply` does on the way in, so an export re-imports unchanged.
            rotation: Some([
                p.rotation.x.to_degrees(),
                p.rotation.y.to_degrees(),
                p.rotation.z.to_degrees(),
            ]),
            scale: Some(p.scale.to_array()),
            color: Some(p.color),
            texture_id: Some(p.texture_id.clone()),
            model: Some(p.model.clone()),
            surface: Some(p.surface),
            geometry: Some(PrimGeometry {
                path_cut_begin: p.path_cut_begin,
                path_cut_end: p.path_cut_end,
                hollow: p.hollow,
                twist_begin: p.twist_begin,
                twist_end: p.twist_end,
                taper_x: p.taper_x,
                taper_y: p.taper_y,
                top_shear_x: p.top_shear_x,
                top_shear_y: p.top_shear_y,
                slice_begin: p.slice_begin,
                slice_end: p.slice_end,
            }),
            particles: Some(p.particles.as_ref().map(ParticleSpec::of)),
            script,
        }
    }
}

impl ParticleSpec {
    /// A live emitter, spelled out in full.
    ///
    /// Every field is named rather than only the ones that differ from the
    /// preset: a document has to survive a preset being retuned later, and "the
    /// fire preset's speed" is not a stable thing to write down.
    #[must_use]
    pub fn of(e: &ParticleEffect) -> Self {
        Self {
            burst: e.burst,
            count: Some(e.count),
            life: Some(e.life),
            speed: Some(e.speed),
            spread_deg: Some(e.spread_deg),
            gravity: Some(e.gravity),
            source_radius: Some(e.source_radius),
            size_start: Some(e.size_start),
            size_end: Some(e.size_end),
            color_start: Some(e.color_start),
            color_end: Some(e.color_end),
            alpha: Some(e.alpha),
            turbulence: Some(e.turbulence),
            sequence: Some(e.sequence),
        }
    }
}

/// What a prim looks like on the way out: the authoritative state plus the two
/// things an author asked about that are not in it.
#[derive(Debug, Serialize)]
struct PrimView {
    #[serde(flatten)]
    prim: PrimDto,
    /// Whether the host is currently running a script for this prim. Not the same
    /// as "has a script": a source that failed to compile is stored and not
    /// running, and that distinction is the first thing an author needs.
    scripted: bool,
    /// The source, on the single-prim route only — it is an order of magnitude
    /// larger than the rest of a prim, which is why `WorldSnapshot` has never
    /// carried it.
    #[serde(skip_serializing_if = "Option::is_none")]
    script: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// What the world holds and what it may hold — the operator's view of the same
/// numbers `NetMessage::WorldCapacity` pushes to every client.
///
/// Replaces `GET /api/admin/regions`, which listed one row per region with its
/// budget on it. There are no regions; the budget is the server's, and it is set
/// in `cartopolis.toml` (`max_prims` / `max_scripts`).
async fn get_capacity(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&state, &headers)?;
    let usage = state.world.read().await.capacity();
    Ok(Json(json!({
        "prims_used": usage.prims_used,
        "prims_max": usage.prims_max,
        "scripts_used": usage.scripts_used,
        "scripts_max": usage.scripts_max,
    })))
}

async fn list_prims(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&state, &headers)?;
    let w = state.world.read().await;
    // Source is left out here on purpose: a hundred prims' worth of Luau is not
    // what "list the prims" means, and the single-prim route has it.
    let prims: Vec<PrimView> = w
        .prims()
        .iter()
        .map(|p| PrimView {
            prim: p.clone(),
            scripted: w.is_scripted(p.id),
            script: None,
        })
        .collect();
    Ok(Json(json!({ "prims": prims })))
}

async fn get_prim(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<PrimView>> {
    require_admin(&state, &headers)?;
    let (prim, scripted) = {
        let w = state.world.read().await;
        let prim = w.prim(id).cloned().ok_or_else(not_found)?;
        (prim, w.is_scripted(id))
    };
    let script = crate::http::with_db(&state, |c| crate::db::get_prim_script(c, id))?;
    Ok(Json(PrimView {
        prim,
        scripted,
        script,
    }))
}

async fn create_prim(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(spec): Json<PrimSpec>,
) -> ApiResult<(StatusCode, Json<PrimView>)> {
    let (_, who) = require_admin(&state, &headers)?;
    let created = create(&state, &spec).await?;
    tracing::info!(%who, prim_id = created.id, "prim authored");
    let scripted = store_script(&state, created.id, spec.script.as_deref()).await?;
    Ok((
        StatusCode::CREATED,
        Json(PrimView {
            prim: created,
            scripted,
            script: None,
        }),
    ))
}

/// Make one prim from a spec, validated, persisted and announced.
///
/// Split out of the handler so a content pack installs prims through *exactly*
/// the code an author's `POST` runs — same validation, same defaults, same
/// rollback, same broadcast. A pack that had its own create would be a second
/// place for "what does an unmentioned field mean?" to be answered, and the two
/// answers would drift.
///
/// The script is deliberately **not** stored here: it draws against a different
/// budget, and the caller wants to report the prim even when the script is
/// refused.
async fn create(state: &HttpState, spec: &PrimSpec) -> ApiResult<PrimDto> {
    // A prim has to be *somewhere*, and there is no sensible default for where:
    // the sim holds no map, so its own frame origin is not a place.
    let (Some(latitude), Some(longitude)) = (spec.latitude, spec.longitude) else {
        return Err(bad("latitude and longitude are required to create a prim"));
    };
    let geo = check_geo(latitude, longitude)?;
    let shape = spec.shape.clone().unwrap_or_else(|| "box".to_string());
    check_shape(&shape)?;

    let created = {
        let mut w = state.world.write().await;
        let base = w
            .add_prim(
                Vec3::new(0.0, spec.altitude.unwrap_or(0.0), 0.0),
                &shape,
                geo,
                // Not the resolved effect: the insert only needs *a* prim to
                // exist, and `apply` below writes every authored field onto it in
                // one update. Passing it twice would mean two places deciding what
                // an unmentioned field means.
                None,
                None,
            )
            .map_err(bad)?;
        // The insert takes shape, altitude and place; everything else is an
        // update on top, which is the same two-step the wire's `CreatePrim` does
        // and for the same reason — `insert_prim` writes defaults, and there is
        // one function that writes the rest.
        apply(&mut w, &base, spec).map_err(|e| {
            // Roll the empty prim back rather than leaving a grey box named
            // "Prim" behind: a create either happened or it did not.
            let _ = w.remove_prim(base.id);
            bad(e)
        })?
    };
    broadcast_upsert(state, &created);
    Ok(created)
}

async fn update_prim(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(spec): Json<PrimSpec>,
) -> ApiResult<Json<PrimView>> {
    let (_, who) = require_admin(&state, &headers)?;
    if let Some(s) = &spec.shape {
        check_shape(s)?;
    }
    let updated = {
        let mut w = state.world.write().await;
        let current = w.prim(id).cloned().ok_or_else(not_found)?;
        apply(&mut w, &current, &spec).map_err(bad)?
    };
    broadcast_upsert(&state, &updated);
    tracing::info!(%who, prim_id = id, "prim edited");

    let scripted = store_script(&state, id, spec.script.as_deref()).await?;
    Ok(Json(PrimView {
        prim: updated,
        scripted,
        script: None,
    }))
}

async fn delete_prim(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let (_, who) = require_admin(&state, &headers)?;
    let gone = {
        let mut w = state.world.write().await;
        w.remove_prim(id).map_err(bad)?
    };
    if !gone {
        return Err(not_found());
    }
    // The VM goes with it. `remove_prim` dropped the source row and released the
    // budget, but the host holds its own map keyed by prim id and would keep
    // running a script for a prim nobody can see.
    state.scripts.unload(id);
    if let Ok(frame) = encode_app_frame(&NetMessage::PrimRemoved { id }) {
        let _ = state.tx_snap.send(frame);
    }
    tracing::info!(%who, prim_id = id, "prim deleted");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ScriptBody {
    /// Luau source. Empty removes the script.
    source: String,
}

async fn put_script(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<ScriptBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let (_, who) = require_admin(&state, &headers)?;
    let scripted = store_script(&state, id, Some(&body.source)).await?;
    tracing::info!(%who, prim_id = id, scripted, "prim script stored");
    // A compile error arrives asynchronously, as a `ScriptOutput` on the wire —
    // the host compiles on its own thread, and blocking this request until it
    // answered would put an HTTP handler behind a script's load budget. Saying so
    // is better than implying the script is running.
    Ok(Json(json!({
        "stored": true,
        "scripted": scripted,
        "note": "compile errors and vfLog output arrive on the wire as ScriptOutput, \
                 not in this response",
    })))
}

// ---------------------------------------------------------------------------
// Import and export — a person's own build, as a document
// ---------------------------------------------------------------------------

/// What a caller may take away and bring back.
///
/// It is a list of [`PrimSpec`] — the *same* type the create route accepts — so
/// an export is literally a document you can post back, and there is no second
/// description of a prim to keep in step with the first. `content.rs` makes the
/// same choice for the packs the server carries, and for the same reason.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContentDocument {
    /// Bumped only if the shape stops being readable by an older importer. A
    /// document is a file somebody keeps, so it has to say what it is.
    pub version: u32,
    /// Free text from the exporter — who and when. Ignored on import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exported_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exported_at: Option<String>,
    pub prims: Vec<PrimSpec>,
}

/// The only document version this build writes.
const CONTENT_DOCUMENT_VERSION: u32 = 1;

/// Most prims one import may create. A document is a file a person can edit, so
/// this is the ceiling on what one request may cost the server — the world's own
/// prim budget still applies underneath and is the real limit.
const MAX_IMPORT_PRIMS: usize = 512;

/// Whose build to export. Absent means the caller's own.
#[derive(Deserialize, Default)]
struct ExportQuery {
    /// An account's username. Admin only.
    owner: Option<String>,
    /// Everything in the world, unowned prims included. Admin only.
    #[serde(default)]
    all: bool,
}

/// A build, as a document.
///
/// **A player is scoped to their own; an admin is not scoped at all.** Those are
/// two rules, and they are not in tension: the default for everybody is
/// `prims.owner = me`, so an ordinary account cannot read somebody else's work
/// out of this route by any argument it can supply. Reaching past that needs
/// `?owner=` or `?all=true`, and both are refused without an admin flag — the
/// operator who can already read `world.db` over ssh and `GET /api/admin/prims`
/// loses nothing by being able to do it here too, and gains the one thing this
/// route is actually for: taking a copy of the world before changing it.
///
/// A prim with no owner — everything built before schema V15, and everything a
/// content pack installed — is not in anybody's own export. `?all=true` is the
/// only way to reach it, which is right: it belongs to the server, and the server
/// is what an operator administers.
async fn export_content(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Query(q): Query<ExportQuery>,
) -> ApiResult<Json<ContentDocument>> {
    let (principal, username) = crate::http::require_session(&state, &headers)?;
    let wants_others = q.all || q.owner.is_some();
    if wants_others && !crate::http::is_caller_admin(&state, principal)? {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "exporting another account's build needs an admin".into(),
        ));
    }

    let rows = if q.all {
        crate::http::with_db(&state, crate::db::all_prims_with_scripts)?
    } else {
        // Resolved to a uuid first, so a username that does not exist is a 404
        // rather than an empty document that reads as "they have built nothing".
        let target = match &q.owner {
            None => principal.to_string(),
            Some(name) => crate::http::with_db(&state, |c| crate::db::enabled_account_id(c, name))?
                .ok_or_else(|| {
                    ApiError(StatusCode::NOT_FOUND, format!("no account called `{name}`"))
                })?,
        };
        crate::http::with_db(&state, |c| crate::db::owned_prims(c, &target))?
    };

    let prims = rows
        .into_iter()
        .map(|(p, script)| PrimSpec::of(&p, script))
        .collect::<Vec<_>>();
    tracing::info!(%username, count = prims.len(), all = q.all, owner = ?q.owner, "content exported");
    Ok(Json(ContentDocument {
        version: CONTENT_DOCUMENT_VERSION,
        exported_by: Some(username),
        exported_at: Some(crate::db::now_iso8601()),
        prims,
    }))
}

#[derive(Deserialize)]
struct TransferRequest {
    /// The prims to hand over.
    prims: Vec<i64>,
    /// The username to hand them to.
    to: String,
}

/// Give prims to another account.
///
/// Two callers may do it and they are allowed for different reasons: the **owner**,
/// because giving your own work away is yours to decide, and an **admin**, because
/// somebody has to be able to re-home a build whose author has gone.
///
/// Unowned prims can only be moved by an admin — there is no owner to consent, and
/// the server is what an operator administers. An all-or-nothing check: the
/// permission is verified for every prim in the list *before* anything is written,
/// so a request naming one prim you own and one you do not moves neither. A partial
/// transfer would be the worst outcome, because the caller has no way to tell from
/// the response which half happened.
async fn transfer_content(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<TransferRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let (principal, username) = crate::http::require_session(&state, &headers)?;
    if body.prims.is_empty() {
        return Err(bad("name at least one prim to transfer"));
    }
    let admin = crate::http::is_caller_admin(&state, principal)?;
    let to = crate::http::with_db(&state, |c| crate::db::enabled_account_id(c, &body.to))?
        .ok_or_else(|| {
            ApiError(
                StatusCode::NOT_FOUND,
                format!("no enabled account called `{}`", body.to),
            )
        })?;

    // Check everything first; write nothing until all of it passes.
    let me = principal.to_string();
    for id in &body.prims {
        let owner = crate::http::with_db(&state, |c| crate::db::prim_owner(c, *id))?;
        if state.world.read().await.prim(*id).is_none() {
            return Err(ApiError(StatusCode::NOT_FOUND, format!("no prim {id}")));
        }
        if admin {
            continue;
        }
        match owner {
            Some(o) if o == me => {}
            Some(_) => {
                return Err(ApiError(
                    StatusCode::FORBIDDEN,
                    format!("prim {id} is not yours to give away"),
                ))
            }
            None => {
                return Err(ApiError(
                    StatusCode::FORBIDDEN,
                    format!("prim {id} belongs to the server; only an admin may re-home it"),
                ))
            }
        }
    }

    for id in &body.prims {
        crate::http::with_db(&state, |c| crate::db::set_prim_owner(c, *id, &to))?;
        state.world.write().await.set_owner(*id, &to);
    }
    tracing::info!(%username, count = body.prims.len(), to = %body.to, "ownership transferred");
    Ok(Json(json!({ "transferred": body.prims, "to": body.to })))
}

/// Create everything in a document, owned by the caller.
///
/// Import **adds**; it does not replace. Two reasons. A document has no way to
/// say which live prim each of its entries *was* — ids are the server's and are
/// not carried, deliberately, because an id from another server means nothing —
/// so "replace" would have to guess, and guessing wrong deletes somebody's work.
/// And importing your own export twice is a reasonable thing to want (a second
/// copy of a build, somewhere else), which a replacing import makes impossible.
///
/// Deleting the old copy first is one `DELETE /api/admin/prims/{id}` per prim and
/// is the caller's decision to make explicitly.
async fn import_content(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(doc): Json<ContentDocument>,
) -> ApiResult<Json<serde_json::Value>> {
    let (principal, username) = crate::http::require_session(&state, &headers)?;
    if doc.version > CONTENT_DOCUMENT_VERSION {
        return Err(bad(format!(
            "this document is version {} and this server reads up to {CONTENT_DOCUMENT_VERSION}",
            doc.version
        )));
    }
    if doc.prims.is_empty() {
        return Err(bad("the document has no prims in it"));
    }
    if doc.prims.len() > MAX_IMPORT_PRIMS {
        return Err(bad(format!(
            "{} prims in one import; the limit is {MAX_IMPORT_PRIMS}",
            doc.prims.len()
        )));
    }

    let mut created = Vec::new();
    for spec in &doc.prims {
        // Straight through `create`, so an imported prim is checked exactly as
        // one an author posts by hand: same shape validation, same geo bounds,
        // same world budget. A document cannot describe a prim the API would
        // otherwise refuse.
        let prim = create(&state, spec).await?;
        let owner = principal.to_string();
        crate::http::with_db(&state, |c| crate::db::set_prim_owner(c, prim.id, &owner))?;
        // The world's map too, or the importer could not edit what they had just
        // imported until the server restarted (`SimWorld::may_edit`).
        state.world.write().await.set_owner(prim.id, &owner);
        store_script(&state, prim.id, spec.script.as_deref()).await?;
        created.push(json!({ "id": prim.id, "name": prim.name }));
    }
    tracing::info!(%username, count = created.len(), "content imported");
    Ok(Json(json!({
        "imported": created,
        "note": "import adds rather than replaces; compile errors arrive on the wire \
                 as ScriptOutput, not in this response",
    })))
}

// ---------------------------------------------------------------------------
// Content packs
// ---------------------------------------------------------------------------

async fn list_content(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&state, &headers)?;
    let packs: Vec<serde_json::Value> = crate::content::packs()
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "summary": p.summary,
                "prims": p.prims.iter().filter_map(|s| s.name.clone()).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(Json(json!({ "content": packs })))
}

/// Install a named pack, replacing anything it installed before.
///
/// It goes through [`create`] prim by prim, which is what keeps a pack from being
/// a second way to make a prim: whatever the API refuses an author, it refuses a
/// pack. The replace step is not a nicety — without it, running this twice leaves
/// two firebirds circling the same city and no obvious way to tell which is which.
async fn install_content(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let (_, who) = require_admin(&state, &headers)?;
    let pack = crate::content::pack(&name).ok_or_else(|| {
        ApiError(
            StatusCode::NOT_FOUND,
            format!("no content called `{name}`; GET /api/admin/content lists what there is"),
        )
    })?;

    // Out with the old — by *provenance*, not by name.
    //
    // Matching on the prim names a pack declares was the first version and it is
    // broken by the pack itself: the beacon's script renames it to whatever its
    // label currently says, so a reinstall found no prim called "Beacon", left the
    // old one standing and added a second. A script may rewrite any field a client
    // can see; `prims.origin` is one it cannot reach.
    let doomed = crate::http::with_db(&state, |c| crate::db::prim_ids_from(c, &pack.name))?;
    let replaced = {
        let mut w = state.world.write().await;
        let mut gone = Vec::new();
        for id in doomed {
            if w.remove_prim(id).unwrap_or(false) {
                gone.push(id);
            }
        }
        gone
    };
    for id in &replaced {
        // The VM goes with the prim, and so does the client's copy — a snapshot
        // is AoI-filtered and carries no removals.
        state.scripts.unload(*id);
        if let Ok(frame) = encode_app_frame(&NetMessage::PrimRemoved { id: *id }) {
            let _ = state.tx_snap.send(frame);
        }
    }

    let mut installed = Vec::new();
    for spec in &pack.prims {
        let prim = create(&state, spec).await?;
        // Stamped straight after the create, so a pack that fails partway through
        // still leaves every prim it did make reachable by the next reinstall
        // rather than as litter nothing owns.
        crate::http::with_db(&state, |c| {
            crate::db::set_prim_origin(c, prim.id, &pack.name)
        })?;
        installed.push(json!({ "id": prim.id, "name": prim.name }));
        store_script(&state, prim.id, spec.script.as_deref()).await?;
    }
    tracing::info!(%who, pack = %name, replaced = replaced.len(), "content installed");
    Ok(Json(json!({
        "installed": installed,
        "replaced": replaced,
        "note": "compile errors and vfLog output arrive on the wire as ScriptOutput, \
                 not in this response",
    })))
}

// ---------------------------------------------------------------------------
// The shared half
// ---------------------------------------------------------------------------

/// Merge a spec onto a prim and write it through the world.
///
/// Takes the *current* prim as the base rather than reading it again, so a create
/// (base = the freshly inserted defaults) and a patch (base = what is there) are
/// one code path. Every field an author did not mention keeps its value, which is
/// what makes `PATCH {"color": [1,0,0]}` a colour change rather than a reset of
/// everything else to a default.
fn apply(
    w: &mut crate::state::SimWorld,
    base: &PrimDto,
    spec: &PrimSpec,
) -> Result<PrimDto, String> {
    let geo = match (spec.latitude, spec.longitude) {
        (None, None) => base.geo,
        (lat, lng) => {
            let g = GeoAnchor {
                latitude: lat.unwrap_or(base.geo.latitude),
                longitude: lng.unwrap_or(base.geo.longitude),
            };
            check_geo(g.latitude, g.longitude).map_err(|e| e.1)?;
            g
        }
    };
    let position = Vec3::new(
        base.position.x,
        spec.altitude.unwrap_or(base.position.y),
        base.position.z,
    );
    let rotation = match spec.rotation {
        Some(d) => Vec3::new(d[0].to_radians(), d[1].to_radians(), d[2].to_radians()),
        None => base.rotation,
    };
    let scale = spec.scale.map_or(base.scale, Vec3::from_array);
    let color = spec.color.unwrap_or(base.color);
    // `Some(None)` is an explicit null — take it off. `None` is absent — leave it.
    let texture_id = match &spec.texture_id {
        Some(v) => v.clone(),
        None => base.texture_id.clone(),
    };
    let model = match &spec.model {
        Some(v) => v.clone(),
        None => base.model.clone(),
    };
    let particles = match &spec.particles {
        Some(v) => v.as_ref().map(|s| s.resolve(base.particles.as_ref())),
        None => base.particles,
    };
    let name = spec.name.clone().unwrap_or_else(|| base.name.clone());
    let surface = spec.surface.unwrap_or(base.surface);
    let geometry = spec.geometry.unwrap_or_else(|| PrimGeometry {
        path_cut_begin: base.path_cut_begin,
        path_cut_end: base.path_cut_end,
        hollow: base.hollow,
        twist_begin: base.twist_begin,
        twist_end: base.twist_end,
        taper_x: base.taper_x,
        taper_y: base.taper_y,
        top_shear_x: base.top_shear_x,
        top_shear_y: base.top_shear_y,
        slice_begin: base.slice_begin,
        slice_end: base.slice_end,
    });
    // The shape is the one field `update_prim` does not carry — it is written by
    // the insert. Changing it on a patch would need a column write of its own,
    // and nothing has asked for it; refusing beats accepting it silently.
    if spec.shape.as_deref().is_some_and(|s| s != base.shape) {
        return Err(format!(
            "this prim is a {}; a shape cannot be changed after it is created",
            base.shape
        ));
    }
    w.update_prim(
        base.id,
        position,
        rotation,
        scale,
        color,
        texture_id,
        &name,
        surface,
        geometry,
        geo,
        particles.as_ref(),
        model.as_deref(),
    )
}

/// Store (or, with an empty source, remove) a prim's script, and tell the host.
///
/// Returns whether the prim now carries one. `None` means the request said
/// nothing about the script, which is not the same as an empty one — that
/// distinction is why a `PATCH` that only changes a colour does not silently
/// delete the script attached to the same prim.
async fn store_script(state: &HttpState, prim_id: i64, source: Option<&str>) -> ApiResult<bool> {
    let Some(source) = source else {
        return Ok(state.world.read().await.is_scripted(prim_id));
    };
    // Trimmed first, for the reason the wire's `SetPrimScript` trims: a buffer of
    // whitespace means "no script", and the two differ in whether they draw
    // against the world's budget.
    let source = source.trim();
    if source.len() > crate::scripting::MAX_SOURCE_BYTES {
        return Err(bad(format!(
            "script is {} bytes; the limit is {}",
            source.len(),
            crate::scripting::MAX_SOURCE_BYTES
        )));
    }
    let mut w = state.world.write().await;
    if !w.has_prim(prim_id) {
        return Err(not_found());
    }
    if source.is_empty() {
        crate::http::with_db(state, |c| crate::db::delete_prim_script(c, prim_id))?;
        w.set_scripted(prim_id, false);
        state.scripts.unload(prim_id);
        return Ok(false);
    }
    w.can_add_script(prim_id).map_err(bad)?;
    crate::http::with_db(state, |c| crate::db::set_prim_script(c, prim_id, source))?;
    w.set_scripted(prim_id, true);
    state.scripts.load(prim_id, source.to_string());
    Ok(true)
}

fn broadcast_upsert(state: &HttpState, prim: &PrimDto) {
    match encode_app_frame(&NetMessage::PrimUpsert { prim: prim.clone() }) {
        Ok(frame) => {
            let _ = state.tx_snap.send(frame);
        }
        Err(e) => tracing::error!("authoring upsert encode: {e}"),
    }
}

/// The shapes the client can mesh (`components::PrimShape`).
///
/// Checked here rather than left to the renderer because the renderer's
/// `from_str` falls back to a box: a typo'd `"spehre"` would be accepted, stored,
/// and drawn as a cube, with the API having reported success.
const SHAPES: &[&str] = &["box", "sphere", "cylinder", "cone", "torus"];

fn check_shape(shape: &str) -> ApiResult<()> {
    if !SHAPES.contains(&shape) {
        return Err(bad(format!(
            "`{shape}` is not a shape; try one of: {}",
            SHAPES.join(", ")
        )));
    }
    Ok(())
}

fn check_geo(lat: f64, lng: f64) -> ApiResult<GeoAnchor> {
    if !lat.is_finite() || !lng.is_finite() || lat.abs() > 90.0 || lng.abs() > 180.0 {
        return Err(bad(
            "latitude must be in -90..90 and longitude in -180..180",
        ));
    }
    Ok(GeoAnchor {
        latitude: lat,
        longitude: lng,
    })
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "no such prim".into())
}
