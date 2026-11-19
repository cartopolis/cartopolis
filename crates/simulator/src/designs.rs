//! Designs: a group of objects built in the design room, kept in a person's
//! inventory and placed again as copies (`docs/mockups/design-room/README.md`).
//!
//! A design is the document `/api/content/export` writes — a list of
//! [`PrimSpec`], the same type the create route takes — plus the spot it was
//! designed for. Placing it somewhere else moves every part by the distance
//! between that spot and the new one, **in metres**, so a stall drawn at the
//! Grote Markt is the same stall at the Vismarkt and not one squashed by the
//! longitude scale of another latitude.
//!
//! Saving reads the parts from the database rather than taking them from the
//! client: a design can only hold prims the caller owns, as they are now, so a
//! client cannot put somebody else's work — or a prim that never existed — into
//! its inventory. Placing goes through `authoring::create` one part at a time,
//! the path an import takes, so a copy meets the same permission, shape and
//! world-budget checks as anything else that is built.
//!
//! The routes are under `/api/inventory`, which `cartopolis.org` already proxies
//! to the simulator (`model-assets-and-inventory.md`); a prefix of their own
//! would need a Caddy change before any client could reach them.

use crate::authoring::{create, store_script, ContentDocument, PrimSpec, CONTENT_DOCUMENT_VERSION};
use crate::http::{internal, ApiError, ApiResult, HttpState};
use crate::prims::Actor;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use cartopolis_core::GroupLevel;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};

/// Most parts one design may hold. The mockup's answer to "how big can a design
/// be?" — a bound per design, so one placement cannot flood a square.
pub const MAX_DESIGN_PARTS: usize = 64;

/// Longest a design's name may be, in characters.
const MAX_NAME_CHARS: usize = 80;

/// Metres per degree of latitude, on the sphere the client's own
/// `geo_nav::world_to_lat_lng` uses. Only differences of a few metres are ever
/// converted, so the sphere is exact enough.
const METRES_PER_DEGREE: f64 = 6_378_137.0 * std::f64::consts::PI / 180.0;

pub fn routes() -> Router<HttpState> {
    Router::new()
        .route(
            "/api/inventory/designs",
            get(list_designs).post(save_design),
        )
        .route("/api/inventory/designs/{id}", delete(delete_design))
        .route("/api/inventory/designs/{id}/place", post(place_design))
        .route("/api/inventory/drafts/place", post(place_drafts))
        .route("/api/inventory/groups", post(make_group))
        .route(
            "/api/inventory/groups/{id}",
            patch(rename_group).delete(dissolve_group),
        )
}

/// One design, as the inventory lists it. The document stays on the server.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DesignItem {
    pub id: i64,
    pub name: String,
    pub parts: i64,
    pub latitude: f64,
    pub longitude: f64,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
struct SaveBody {
    name: String,
    /// The spot the design was made for: the room's centre.
    latitude: f64,
    longitude: f64,
    /// The objects in the room. Only those the caller owns are kept.
    prim_ids: Vec<i64>,
}

#[derive(Debug, Deserialize)]
struct PlaceBody {
    latitude: f64,
    longitude: f64,
    /// Place the copy as a design-room draft only the caller is sent.
    #[serde(default)]
    draft: bool,
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

fn check_spot(lat: f64, lng: f64) -> ApiResult<()> {
    if !lat.is_finite() || !lng.is_finite() || lat.abs() > 85.0 || lng.abs() > 180.0 {
        return Err(bad(
            "latitude must be in -85..85 and longitude in -180..180",
        ));
    }
    Ok(())
}

/// Move a point designed relative to `from` so it stands the same metres east
/// and north of `to`.
#[must_use]
pub fn shift(point: (f64, f64), from: (f64, f64), to: (f64, f64)) -> (f64, f64) {
    let north_m = (point.0 - from.0) * METRES_PER_DEGREE;
    let east_m = (point.1 - from.1) * METRES_PER_DEGREE * from.0.to_radians().cos();
    (
        to.0 + north_m / METRES_PER_DEGREE,
        to.1 + east_m / (METRES_PER_DEGREE * to.0.to_radians().cos()),
    )
}

/// `GET /api/inventory/designs` — the caller's designs, newest first.
async fn list_designs(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<DesignItem>>> {
    let (principal, _) = crate::http::require_session(&state, &headers)?;
    let owner = principal.to_string();
    let items = crate::http::with_db(&state, |c| {
        let mut stmt = c.prepare(
            "SELECT id, name, parts, origin_lat, origin_lng, created_at FROM designs \
             WHERE account_id = ?1 ORDER BY id DESC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![owner], |r| {
                Ok(DesignItem {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    parts: r.get(2)?,
                    latitude: r.get(3)?,
                    longitude: r.get(4)?,
                    created_at: r.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    Ok(Json(items))
}

/// `POST /api/inventory/designs` — keep the caller's objects in the room as one
/// design.
async fn save_design(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<SaveBody>,
) -> ApiResult<Json<DesignItem>> {
    let (principal, username) = crate::http::require_session(&state, &headers)?;
    check_spot(body.latitude, body.longitude)?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(bad("a design needs a name"));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(bad(format!(
            "a name is at most {MAX_NAME_CHARS} characters"
        )));
    }
    let owner = principal.to_string();
    let wanted: std::collections::HashSet<i64> = body.prim_ids.iter().copied().collect();
    let rows = crate::http::with_db(&state, |c| crate::db::owned_prims(c, &owner))?;
    let prims: Vec<PrimSpec> = rows
        .into_iter()
        .filter(|(p, _)| wanted.contains(&p.id))
        .map(|(p, script)| PrimSpec::of(&p, script))
        .collect();
    if prims.is_empty() {
        return Err(bad(
            "nothing in the room is yours to keep: build something here first",
        ));
    }
    if prims.len() > MAX_DESIGN_PARTS {
        return Err(bad(format!(
            "{} objects in one design; the limit is {MAX_DESIGN_PARTS}",
            prims.len()
        )));
    }
    let parts = prims.len() as i64;
    let document = serde_json::to_string(&ContentDocument {
        version: CONTENT_DOCUMENT_VERSION,
        exported_by: Some(username.clone()),
        exported_at: Some(crate::db::now_iso8601()),
        prims,
    })
    .map_err(|e| internal(anyhow::anyhow!(e)))?;
    let name = name.to_string();
    let (lat, lng) = (body.latitude, body.longitude);
    let item = crate::http::with_db(&state, move |c| {
        c.execute(
            "INSERT INTO designs (account_id, name, origin_lat, origin_lng, parts, document) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![owner, name, lat, lng, parts, document],
        )?;
        let id = c.last_insert_rowid();
        let created_at: String =
            c.query_row("SELECT created_at FROM designs WHERE id = ?1", [id], |r| {
                r.get(0)
            })?;
        Ok(DesignItem {
            id,
            name,
            parts,
            latitude: lat,
            longitude: lng,
            created_at,
        })
    })?;
    tracing::info!(%username, id = item.id, parts, "design saved");
    Ok(Json(item))
}

/// `POST /api/inventory/designs/{id}/place` — a copy of the design, standing
/// at the given spot and owned by the caller. The design stays in the
/// inventory: placing makes a copy, as Second Life's rez does.
async fn place_design(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<PlaceBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let access = crate::http::require_access(&state, &headers)?;
    check_spot(body.latitude, body.longitude)?;
    let owner = access.principal.to_string();
    let found = crate::http::with_db(&state, |c| {
        use rusqlite::OptionalExtension;
        Ok(c.query_row(
            "SELECT origin_lat, origin_lng, document FROM designs \
             WHERE id = ?1 AND account_id = ?2",
            rusqlite::params![id, owner],
            |r| {
                Ok((
                    r.get::<_, f64>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?)
    })?;
    // Somebody else's design is "not found", not "forbidden": its existence is
    // not the caller's to learn.
    let Some((origin_lat, origin_lng, document)) = found else {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such design".into()));
    };
    let doc: ContentDocument =
        serde_json::from_str(&document).map_err(|e| internal(anyhow::anyhow!(e)))?;
    let perms = crate::http::access_permissions(&state, &access)?;
    let builder = Actor::with_permissions(owner, perms);
    let mut created = Vec::new();
    // A copy is its own group, not a second home for the original's: every
    // group id in the design gets a fresh one, the same for all its parts.
    let mut fresh: HashMap<i64, i64> = HashMap::new();
    for spec in &doc.prims {
        let mut spec = spec.clone();
        if let (Some(lat), Some(lng)) = (spec.latitude, spec.longitude) {
            let (lat, lng) = shift(
                (lat, lng),
                (origin_lat, origin_lng),
                (body.latitude, body.longitude),
            );
            spec.latitude = Some(lat);
            spec.longitude = Some(lng);
        }
        spec.draft = Some(body.draft);
        if let Some(groups) = spec.groups.as_mut() {
            for g in groups.iter_mut() {
                g.id = *fresh.entry(g.id).or_insert_with(new_group_id);
            }
        }
        let prim = create(&state, &builder, &spec).await?;
        store_script(&state, &builder, prim.id, spec.script.as_deref()).await?;
        created.push(prim.id);
    }
    tracing::info!(username = %access.username, id, parts = created.len(), "design placed");
    Ok(Json(json!({ "placed": created })))
}

#[derive(Debug, Deserialize)]
struct PlaceDraftsBody {
    prim_ids: Vec<i64>,
}

/// `POST /api/inventory/drafts/place` — the design room's Place: the caller's
/// drafts among these ids stop being theirs alone and stand in the street for
/// everybody, in one request. Answers the ids that were placed; an id that is
/// not the caller's draft is skipped rather than failing the rest.
async fn place_drafts(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<PlaceDraftsBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let access = crate::http::require_access(&state, &headers)?;
    if body.prim_ids.len() > MAX_DESIGN_PARTS * 4 {
        return Err(bad("too many objects in one placing"));
    }
    let perms = crate::http::access_permissions(&state, &access)?;
    let actor = Actor::with_permissions(access.principal.to_string(), perms);
    let mut placed = Vec::new();
    for id in body.prim_ids {
        match state.prims.place_draft(&actor, id).await {
            Ok(prim) if !prim.draft => placed.push(id),
            Ok(_) => {}
            Err(e) => tracing::debug!(id, "not placed: {e}"),
        }
    }
    tracing::info!(username = %access.username, count = placed.len(), "drafts placed");
    Ok(Json(json!({ "placed": placed })))
}

// ---------------------------------------------------------------------------
// Groups: a chain on each prim, outermost first (`PrimDto::groups`)
// ---------------------------------------------------------------------------

/// A fresh group id: positive, and unlikely ever to meet another on this
/// server — a group is a chain written on its prims, not a row to number.
fn new_group_id() -> i64 {
    (uuid::Uuid::new_v4().as_u64_pair().0 >> 1) as i64
}

fn check_group_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(bad("a group needs a name"));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(bad(format!(
            "a name is at most {MAX_NAME_CHARS} characters"
        )));
    }
    Ok(name.to_string())
}

/// Where each prim's chain goes when `selected` are grouped as `new`.
///
/// The new group goes in at the depth where the selection's chains part: the
/// groups every selected prim is already in stay outside it, and the groups
/// below that point that lie *wholly* inside the selection move inside it, so
/// grouping two groups nests them. A group only partly selected is left behind
/// by the selected prims — otherwise it would stand in two places in the tree.
/// `members` is every prim of each group, selected or not.
#[must_use]
pub fn grouped(
    selected: &[(i64, Vec<GroupLevel>)],
    members: &HashMap<i64, HashSet<i64>>,
    new: &GroupLevel,
) -> Vec<(i64, Vec<GroupLevel>)> {
    let Some((_, first)) = selected.first() else {
        return Vec::new();
    };
    let mut common = first.len();
    for (_, chain) in selected {
        common = common.min(
            chain
                .iter()
                .zip(first)
                .take_while(|(a, b)| a.id == b.id)
                .count(),
        );
    }
    let ids: HashSet<i64> = selected.iter().map(|(id, _)| *id).collect();
    selected
        .iter()
        .map(|(id, chain)| {
            let mut next = chain[..common].to_vec();
            next.push(new.clone());
            next.extend(
                chain[common..]
                    .iter()
                    .filter(|g| members.get(&g.id).is_some_and(|m| m.is_subset(&ids)))
                    .cloned(),
            );
            (*id, next)
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct GroupBody {
    name: String,
    prim_ids: Vec<i64>,
}

/// `POST /api/inventory/groups` — group the caller's prims under a new name
/// (Ctrl-G in the design room). Answers the new group.
async fn make_group(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<GroupBody>,
) -> ApiResult<Json<GroupLevel>> {
    let access = crate::http::require_access(&state, &headers)?;
    let name = check_group_name(&body.name)?;
    if body.prim_ids.is_empty() {
        return Err(bad("nothing to group"));
    }
    let owner = access.principal.to_string();
    let wanted: HashSet<i64> = body.prim_ids.iter().copied().collect();
    let (selected, members) = {
        let w = state.world.read().await;
        let mut selected = Vec::new();
        let mut members: HashMap<i64, HashSet<i64>> = HashMap::new();
        for p in w.prims() {
            if !w.owns(p.id, &owner) {
                continue;
            }
            for g in &p.groups {
                members.entry(g.id).or_default().insert(p.id);
            }
            if wanted.contains(&p.id) {
                selected.push((p.id, p.groups.clone()));
            }
        }
        (selected, members)
    };
    if selected.len() != wanted.len() {
        return Err(bad("only your own objects can be grouped"));
    }
    let new = GroupLevel {
        id: new_group_id(),
        name,
    };
    let changes = grouped(&selected, &members, &new);
    let perms = crate::http::access_permissions(&state, &access)?;
    let actor = Actor::with_permissions(owner, perms);
    state
        .prims
        .set_groups(&actor, changes)
        .await
        .map_err(|e| bad(e.to_string()))?;
    Ok(Json(new))
}

#[derive(Debug, Deserialize)]
struct RenameBody {
    name: String,
}

/// The caller's prims in group `id`, and what each chain becomes under `f`.
async fn regroup(
    state: &HttpState,
    access: &crate::auth::Access,
    id: i64,
    f: impl Fn(&[GroupLevel]) -> Vec<GroupLevel>,
) -> ApiResult<StatusCode> {
    let owner = access.principal.to_string();
    let changes: Vec<(i64, Vec<GroupLevel>)> = {
        let w = state.world.read().await;
        w.prims()
            .iter()
            .filter(|p| w.owns(p.id, &owner) && p.groups.iter().any(|g| g.id == id))
            .map(|p| (p.id, f(&p.groups)))
            .collect()
    };
    if changes.is_empty() {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such group".into()));
    }
    let perms = crate::http::access_permissions(state, access)?;
    let actor = Actor::with_permissions(owner, perms);
    state
        .prims
        .set_groups(&actor, changes)
        .await
        .map_err(|e| bad(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PATCH /api/inventory/groups/{id}` — rename a group.
async fn rename_group(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<RenameBody>,
) -> ApiResult<StatusCode> {
    let access = crate::http::require_access(&state, &headers)?;
    let name = check_group_name(&body.name)?;
    regroup(&state, &access, id, |chain| {
        chain
            .iter()
            .map(|g| GroupLevel {
                id: g.id,
                name: if g.id == id {
                    name.clone()
                } else {
                    g.name.clone()
                },
            })
            .collect()
    })
    .await
}

/// `DELETE /api/inventory/groups/{id}` — ungroup (Alt-G): the group goes, what
/// was in it stays in the groups around it.
async fn dissolve_group(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let access = crate::http::require_access(&state, &headers)?;
    regroup(&state, &access, id, |chain| {
        chain.iter().filter(|g| g.id != id).cloned().collect()
    })
    .await
}

/// `DELETE /api/inventory/designs/{id}` — forget a design. What was placed
/// from it stays standing: those are copies.
async fn delete_design(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let (principal, _) = crate::http::require_session(&state, &headers)?;
    let owner = principal.to_string();
    let gone = crate::http::with_db(&state, |c| {
        Ok(c.execute(
            "DELETE FROM designs WHERE id = ?1 AND account_id = ?2",
            rusqlite::params![id, owner],
        )?)
    })?;
    if gone == 0 {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such design".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shifted_part_keeps_its_metres_from_the_spot() {
        // Two metres north and three east of the Grote Markt…
        let from = (53.2194, 6.5665);
        let point = (
            from.0 + 2.0 / METRES_PER_DEGREE,
            from.1 + 3.0 / (METRES_PER_DEGREE * from.0.to_radians().cos()),
        );
        // …placed in Madrid, where a degree of longitude is longer.
        let to = (40.4168, -3.7038);
        let (lat, lng) = shift(point, from, to);
        let north = (lat - to.0) * METRES_PER_DEGREE;
        let east = (lng - to.1) * METRES_PER_DEGREE * to.0.to_radians().cos();
        assert!((north - 2.0).abs() < 1e-6, "north {north}");
        assert!((east - 3.0).abs() < 1e-6, "east {east}");
    }

    fn g(id: i64, name: &str) -> GroupLevel {
        GroupLevel {
            id,
            name: name.into(),
        }
    }

    fn members(of: &[(i64, &[i64])]) -> HashMap<i64, HashSet<i64>> {
        of.iter()
            .map(|(id, m)| (*id, m.iter().copied().collect()))
            .collect()
    }

    #[test]
    fn loose_parts_go_straight_into_the_new_group() {
        let new = g(9, "Stall");
        let out = grouped(&[(1, vec![]), (2, vec![])], &members(&[]), &new);
        assert_eq!(out, vec![(1, vec![new.clone()]), (2, vec![new])]);
    }

    #[test]
    fn grouping_two_groups_nests_them() {
        let (frame, roof) = (g(1, "Frame"), g(2, "Roof"));
        let new = g(9, "Stall");
        let sel = [
            (10, vec![frame.clone()]),
            (11, vec![frame.clone()]),
            (12, vec![roof.clone()]),
        ];
        let out = grouped(&sel, &members(&[(1, &[10, 11]), (2, &[12])]), &new);
        assert_eq!(out[0].1, vec![new.clone(), frame]);
        assert_eq!(out[2].1, vec![new, roof]);
    }

    #[test]
    fn a_group_inside_a_shared_one_stays_inside_it() {
        let stall = g(1, "Stall");
        let new = g(9, "Frame");
        let sel = [(10, vec![stall.clone()]), (11, vec![stall.clone()])];
        let out = grouped(&sel, &members(&[(1, &[10, 11, 12])]), &new);
        assert_eq!(out[0].1, vec![stall, new]);
    }

    #[test]
    fn a_partly_selected_group_is_left_behind() {
        let frame = g(1, "Frame");
        let new = g(9, "Two posts");
        let sel = [(10, vec![frame.clone()]), (20, vec![])];
        let out = grouped(&sel, &members(&[(1, &[10, 11])]), &new);
        assert_eq!(out[0].1, vec![new.clone()], "post 11 is still in Frame");
        assert_eq!(out[1].1, vec![new]);
    }

    #[test]
    fn the_spot_itself_lands_on_the_new_spot() {
        let (lat, lng) = shift((53.0, 6.0), (53.0, 6.0), (52.0, 5.0));
        assert!((lat - 52.0).abs() < 1e-12 && (lng - 5.0).abs() < 1e-12);
    }
}
