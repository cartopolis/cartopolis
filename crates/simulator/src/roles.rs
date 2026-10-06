//! Roles: who may do what (`docs/notes/permissions.md`, `V27__roles.sql`).
//!
//! The vocabulary is `cartopolis_core::permissions`; this module stores which
//! roles hold which permissions and who holds which roles, and answers the one
//! question every gate asks — [`effective`]: what may this account do *now*.
//!
//! Read from the database at the point of the decision, not carried in a session,
//! for the reason `auth::is_admin` gives: a revoked role takes effect on the next
//! request, not when somebody's token happens to expire. A connection holds its
//! answer for the length of the session instead, and is told when it changes:
//! every route here that changes a permission ends in [`refresh_grants`], which
//! updates the connection's own gate and sends its client
//! `NetMessage::Permissions`.

use std::collections::BTreeMap;

use anyhow::Context;
use cartopolis_core::permissions::{Permission, PermissionSet};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use uuid::Uuid;

/// The role every enabled account holds without being given it.
pub const PLAYER: &str = "player";

/// What an account may do: everything for an enabled admin, nothing for a
/// disabled or missing account, otherwise the player role plus whatever roles
/// it was given.
pub fn effective(conn: &Connection, principal: Uuid) -> anyhow::Result<PermissionSet> {
    let row: Option<(i64, i64)> = conn
        .query_row(
            "SELECT admin, disabled FROM accounts WHERE id = ?1",
            [principal.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((admin, disabled)) = row else {
        return Ok(PermissionSet::none());
    };
    if disabled != 0 {
        return Ok(PermissionSet::none());
    }
    if admin != 0 {
        return Ok(PermissionSet::all());
    }
    let mut stmt = conn.prepare_cached(
        "SELECT permission FROM role_permissions WHERE role_id = ?1
         UNION
         SELECT rp.permission FROM role_permissions rp
           JOIN account_roles ar ON ar.role_id = rp.role_id
          WHERE ar.account_id = ?2",
    )?;
    let slugs = stmt
        .query_map(rusqlite::params![PLAYER, principal.to_string()], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    // A slug no longer in the list (a permission retired since it was granted)
    // grants nothing rather than failing the account.
    Ok(slugs
        .iter()
        .filter_map(|s| Permission::from_slug(s))
        .collect())
}

/// Whether an account holds one permission now.
pub fn has(conn: &Connection, principal: Uuid, p: Permission) -> anyhow::Result<bool> {
    Ok(effective(conn, principal)?.has(p))
}

/// One role, as the web UI lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleRow {
    pub id: String,
    pub name: String,
    pub built_in: bool,
    pub permissions: Vec<String>,
    /// How many accounts hold it explicitly (the player role: none, since every
    /// account holds it implicitly).
    pub members: u64,
}

pub fn list_roles(conn: &Connection) -> anyhow::Result<Vec<RoleRow>> {
    let mut perms: BTreeMap<String, Vec<String>> = BTreeMap::new();
    {
        let mut stmt =
            conn.prepare("SELECT role_id, permission FROM role_permissions ORDER BY permission")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (role, p) = row?;
            perms.entry(role).or_default().push(p);
        }
    }
    let mut stmt = conn.prepare(
        "SELECT r.id, r.name, r.built_in,
                (SELECT COUNT(*) FROM account_roles ar WHERE ar.role_id = r.id)
           FROM roles r ORDER BY r.built_in DESC, r.name",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .map(|(id, name, built_in, members)| {
            // In the catalogue's order, which is the order the web UI lists them.
            let held = perms.remove(&id).unwrap_or_default();
            let permissions = cartopolis_core::permissions::ALL
                .iter()
                .map(|p| p.slug())
                .filter(|s| held.iter().any(|h| h == s))
                .map(str::to_string)
                .collect();
            RoleRow {
                id,
                name,
                built_in: built_in != 0,
                permissions,
                members: members.max(0) as u64,
            }
        })
        .collect())
}

/// Why a role change was refused, in words to show the person who asked.
#[derive(Debug, PartialEq, Eq)]
pub enum RoleError {
    NotFound,
    Invalid(String),
}

/// A role id: lowercase letters, digits and hyphens, 2–32 long.
#[must_use]
pub fn valid_id(id: &str) -> bool {
    (2..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.starts_with('-')
}

/// Parse a permission list, refusing a slug the catalogue does not hold.
pub fn parse_permissions(slugs: &[String]) -> Result<Vec<Permission>, RoleError> {
    slugs
        .iter()
        .map(|s| {
            Permission::from_slug(s)
                .ok_or_else(|| RoleError::Invalid(format!("there is no permission called {s:?}")))
        })
        .collect()
}

pub fn create_role(
    conn: &mut Connection,
    id: &str,
    name: &str,
    permissions: &[Permission],
) -> anyhow::Result<Result<(), RoleError>> {
    if !valid_id(id) {
        return Ok(Err(RoleError::Invalid(
            "a role's id is 2–32 lowercase letters, digits and hyphens".into(),
        )));
    }
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Ok(Err(RoleError::Invalid(
            "a role's name is 1–64 characters".into(),
        )));
    }
    let exists: bool =
        conn.query_row("SELECT COUNT(*) > 0 FROM roles WHERE id = ?1", [id], |r| {
            r.get(0)
        })?;
    if exists {
        return Ok(Err(RoleError::Invalid(format!(
            "there is already a role called {id}"
        ))));
    }
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO roles (id, name, built_in, created_at) VALUES (?1, ?2, 0, datetime('now'))",
        rusqlite::params![id, name],
    )?;
    write_permissions(&tx, id, permissions)?;
    tx.commit()?;
    Ok(Ok(()))
}

/// Rename a role and/or replace its permissions. `None` leaves a field alone.
pub fn update_role(
    conn: &mut Connection,
    id: &str,
    name: Option<&str>,
    permissions: Option<&[Permission]>,
) -> anyhow::Result<Result<(), RoleError>> {
    let exists: bool =
        conn.query_row("SELECT COUNT(*) > 0 FROM roles WHERE id = ?1", [id], |r| {
            r.get(0)
        })?;
    if !exists {
        return Ok(Err(RoleError::NotFound));
    }
    let tx = conn.transaction()?;
    if let Some(name) = name {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return Ok(Err(RoleError::Invalid(
                "a role's name is 1–64 characters".into(),
            )));
        }
        tx.execute(
            "UPDATE roles SET name = ?2 WHERE id = ?1",
            rusqlite::params![id, name],
        )?;
    }
    if let Some(p) = permissions {
        tx.execute("DELETE FROM role_permissions WHERE role_id = ?1", [id])?;
        write_permissions(&tx, id, p)?;
    }
    tx.commit()?;
    Ok(Ok(()))
}

fn write_permissions(
    conn: &Connection,
    id: &str,
    permissions: &[Permission],
) -> anyhow::Result<()> {
    let mut stmt = conn
        .prepare("INSERT OR IGNORE INTO role_permissions (role_id, permission) VALUES (?1, ?2)")?;
    for p in permissions {
        stmt.execute(rusqlite::params![id, p.slug()])?;
    }
    Ok(())
}

/// Delete a role and every grant of it. The player role cannot be deleted.
pub fn delete_role(conn: &mut Connection, id: &str) -> anyhow::Result<Result<(), RoleError>> {
    let built_in: Option<i64> = conn
        .query_row("SELECT built_in FROM roles WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?;
    match built_in {
        None => return Ok(Err(RoleError::NotFound)),
        Some(b) if b != 0 => {
            return Ok(Err(RoleError::Invalid(
                "a built-in role cannot be deleted; take its permissions away instead".into(),
            )))
        }
        Some(_) => {}
    }
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM account_roles WHERE role_id = ?1", [id])?;
    tx.execute("DELETE FROM role_permissions WHERE role_id = ?1", [id])?;
    tx.execute("DELETE FROM roles WHERE id = ?1", [id])?;
    tx.commit()?;
    Ok(Ok(()))
}

/// The roles an account holds explicitly (not `player`, which every account
/// holds), sorted.
pub fn account_roles(conn: &Connection, account: &str) -> anyhow::Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT role_id FROM account_roles WHERE account_id = ?1 ORDER BY role_id")?;
    let ids = stmt
        .query_map([account], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids)
}

/// Every account's explicit roles, for the accounts list.
pub fn all_account_roles(conn: &Connection) -> anyhow::Result<BTreeMap<String, Vec<String>>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut stmt =
        conn.prepare("SELECT account_id, role_id FROM account_roles ORDER BY role_id")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (a, r) = row?;
        out.entry(a).or_default().push(r);
    }
    Ok(out)
}

/// Replace an account's explicit roles. Refuses a role that does not exist, and
/// ignores `player` (held by everyone already).
pub fn set_account_roles(
    conn: &mut Connection,
    account: &str,
    roles: &[String],
) -> anyhow::Result<Result<(), RoleError>> {
    let exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM accounts WHERE id = ?1",
        [account],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(Err(RoleError::NotFound));
    }
    for role in roles {
        let known: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM roles WHERE id = ?1",
            [role],
            |r| r.get(0),
        )?;
        if !known {
            return Ok(Err(RoleError::Invalid(format!(
                "there is no role called {role:?}"
            ))));
        }
    }
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM account_roles WHERE account_id = ?1", [account])
        .context("clear roles")?;
    for role in roles.iter().filter(|r| r.as_str() != PLAYER) {
        tx.execute(
            "INSERT OR IGNORE INTO account_roles (account_id, role_id) VALUES (?1, ?2)",
            rusqlite::params![account, role],
        )?;
    }
    tx.commit()?;
    Ok(Ok(()))
}

/// Forget an account's roles, when the account is deleted.
pub fn forget_account(conn: &Connection, account: &str) -> anyhow::Result<()> {
    conn.execute("DELETE FROM account_roles WHERE account_id = ?1", [account])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The operator API: `/api/admin/permissions`, `/api/admin/roles`,
// `/api/admin/accounts/{id}/roles`
// ---------------------------------------------------------------------------

use crate::http::{
    caller_permissions, is_operator, require, require_session, with_db, with_db_mut, ApiError,
    ApiResult, HttpState,
};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

pub fn routes() -> Router<HttpState> {
    Router::new()
        .route("/api/admin/permissions", get(list_permissions))
        .route("/api/admin/roles", get(get_roles).post(post_role))
        .route(
            "/api/admin/roles/{id}",
            axum::routing::patch(patch_role).delete(remove_role),
        )
        .route("/api/admin/accounts/{id}/roles", put(put_account_roles))
}

fn refused(e: RoleError) -> ApiError {
    match e {
        RoleError::NotFound => ApiError(StatusCode::NOT_FOUND, "no such role or account".into()),
        RoleError::Invalid(why) => ApiError(StatusCode::BAD_REQUEST, why),
    }
}

/// The catalogue, for the web UI to draw its grid from — so the UI never holds
/// a list of its own to fall behind.
async fn list_permissions(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let (principal, _) = require_session(&state, &headers)?;
    if !is_operator(caller_permissions(&state, principal)?) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "that account may not administer this server".into(),
        ));
    }
    let list: Vec<_> = cartopolis_core::permissions::ALL
        .iter()
        .map(|p| {
            json!({
                "slug": p.slug(),
                "label": p.label(),
                "group": p.group(),
                "player_default": p.player_default(),
            })
        })
        .collect();
    Ok(Json(json!({ "permissions": list })))
}

async fn get_roles(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    // Read by whoever manages accounts too: assigning a role means seeing them.
    let (principal, _) = require_session(&state, &headers)?;
    let perms = caller_permissions(&state, principal)?;
    if !perms.has(Permission::RolesManage) && !perms.has(Permission::AccountsManage) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "that account may not see roles".into(),
        ));
    }
    let roles = with_db(&state, list_roles)?;
    Ok(Json(json!({ "roles": roles })))
}

#[derive(Deserialize)]
struct NewRole {
    id: String,
    name: String,
    #[serde(default)]
    permissions: Vec<String>,
}

async fn post_role(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<NewRole>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let (_, who) = require(&state, &headers, Permission::RolesManage)?;
    let perms = parse_permissions(&body.permissions).map_err(refused)?;
    with_db_mut(&state, |c| create_role(c, &body.id, &body.name, &perms))?.map_err(refused)?;
    tracing::info!(%who, role = %body.id, "role created");
    let roles = with_db(&state, list_roles)?;
    Ok((StatusCode::CREATED, Json(json!({ "roles": roles }))))
}

/// Re-read what every connected account may do and tell the ones whose answer
/// changed (`NetMessage::Permissions`, through `ChatHub::publish_grants`).
///
/// Called after anything that can change a permission — a role's grants, a
/// role deleted, an account's roles, its admin or disabled flag. It re-reads
/// every *connected* account rather than working out which ones the edit
/// touched: an edit to Player touches everybody, and the set is the people
/// online, not the accounts table. A read that fails leaves that account as it
/// was, which is the answer it had a moment ago.
pub(crate) fn refresh_grants(state: &HttpState) {
    for account in state.chat.granted_accounts() {
        let Ok(principal) = Uuid::parse_str(&account) else {
            continue;
        };
        if let Ok(now) = with_db(state, |c| effective(c, principal)) {
            state.chat.publish_grants(&account, now);
        }
    }
}

#[derive(Deserialize)]
struct RolePatch {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    permissions: Option<Vec<String>>,
}

async fn patch_role(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<RolePatch>,
) -> ApiResult<Json<serde_json::Value>> {
    let (_, who) = require(&state, &headers, Permission::RolesManage)?;
    let perms = body
        .permissions
        .as_deref()
        .map(parse_permissions)
        .transpose()
        .map_err(refused)?;
    with_db_mut(&state, |c| {
        update_role(c, &id, body.name.as_deref(), perms.as_deref())
    })?
    .map_err(refused)?;
    tracing::info!(%who, role = %id, "role changed");
    refresh_grants(&state);
    let roles = with_db(&state, list_roles)?;
    Ok(Json(json!({ "roles": roles })))
}

async fn remove_role(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let (_, who) = require(&state, &headers, Permission::RolesManage)?;
    with_db_mut(&state, |c| delete_role(c, &id))?.map_err(refused)?;
    tracing::info!(%who, role = %id, "role deleted");
    refresh_grants(&state);
    let roles = with_db(&state, list_roles)?;
    Ok(Json(json!({ "roles": roles })))
}

#[derive(Deserialize)]
struct AccountRoles {
    roles: Vec<String>,
}

/// Replace the roles an account holds. Roles manage *what* may be done; who is
/// an admin stays the admin flag on the accounts page.
async fn put_account_roles(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<AccountRoles>,
) -> ApiResult<Json<serde_json::Value>> {
    let (_, who) = require(&state, &headers, Permission::RolesManage)?;
    with_db_mut(&state, |c| set_account_roles(c, &id, &body.roles))?.map_err(refused)?;
    tracing::info!(%who, account = %id, roles = ?body.roles, "account roles set");
    refresh_grants(&state);
    let roles = with_db(&state, |c| account_roles(c, &id))?;
    Ok(Json(json!({ "roles": roles })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_core::permissions::ALL;

    fn db() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&mut c).unwrap();
        c
    }

    fn account(c: &Connection, name: &str, admin: bool) -> Uuid {
        crate::auth::create_account_with_role(c, name, "pw-long-enough", admin).unwrap()
    }

    /// The migration's seed and `player_default` are one list written twice.
    #[test]
    fn the_seeded_player_role_is_the_default() {
        let c = db();
        let roles = list_roles(&c).unwrap();
        let player = roles.iter().find(|r| r.id == PLAYER).unwrap();
        let want: Vec<String> = ALL
            .iter()
            .filter(|p| p.player_default())
            .map(|p| p.slug().to_string())
            .collect();
        assert_eq!(player.permissions, want);
        assert!(player.built_in);
    }

    #[test]
    fn a_player_holds_the_player_role_an_admin_everything() {
        let c = db();
        let alice = account(&c, "alice", false);
        let root = account(&c, "root", true);
        let a = effective(&c, alice).unwrap();
        assert!(a.has(Permission::PrimCreate));
        assert!(!a.has(Permission::PrimEditAny));
        assert!(!a.has(Permission::AccountsManage));
        assert_eq!(effective(&c, root).unwrap(), PermissionSet::all());
        assert_eq!(
            effective(&c, Uuid::new_v4()).unwrap(),
            PermissionSet::none()
        );
    }

    #[test]
    fn a_role_adds_its_permissions_and_taking_it_away_removes_them() {
        let mut c = db();
        let bob = account(&c, "bob", false);
        create_role(
            &mut c,
            "builder",
            "Builder",
            &[Permission::PrimEditAny, Permission::BuildingOverride],
        )
        .unwrap()
        .unwrap();
        set_account_roles(&mut c, &bob.to_string(), &["builder".into()])
            .unwrap()
            .unwrap();
        assert!(has(&c, bob, Permission::BuildingOverride).unwrap());
        assert!(
            has(&c, bob, Permission::PrimCreate).unwrap(),
            "still a player"
        );
        set_account_roles(&mut c, &bob.to_string(), &[])
            .unwrap()
            .unwrap();
        assert!(!has(&c, bob, Permission::BuildingOverride).unwrap());
    }

    #[test]
    fn the_player_role_can_be_narrowed_for_everyone() {
        let mut c = db();
        let bob = account(&c, "bob", false);
        update_role(&mut c, PLAYER, None, Some(&[Permission::PrimCreate]))
            .unwrap()
            .unwrap();
        assert!(!has(&c, bob, Permission::ScriptEditOwn).unwrap());
        assert!(has(&c, bob, Permission::PrimCreate).unwrap());
    }

    #[test]
    fn a_disabled_account_holds_nothing() {
        let c = db();
        let bob = account(&c, "bob", true);
        crate::auth::set_disabled(&c, &bob.to_string(), true).unwrap();
        assert_eq!(effective(&c, bob).unwrap(), PermissionSet::none());
    }

    #[test]
    fn the_player_role_cannot_be_deleted_and_unknown_names_are_refused() {
        let mut c = db();
        assert!(matches!(
            delete_role(&mut c, PLAYER).unwrap(),
            Err(RoleError::Invalid(_))
        ));
        assert!(matches!(
            parse_permissions(&["prim_teleport".into()]),
            Err(RoleError::Invalid(_))
        ));
        assert!(!valid_id("Builder"));
        assert!(valid_id("site-moderator"));
        let bob = account(&c, "bob", false);
        assert!(matches!(
            set_account_roles(&mut c, &bob.to_string(), &["nope".into()]).unwrap(),
            Err(RoleError::Invalid(_))
        ));
    }

    #[test]
    fn deleting_a_role_takes_it_from_everyone() {
        let mut c = db();
        let bob = account(&c, "bob", false);
        create_role(&mut c, "mods", "Moderators", &[Permission::ReportsTriage])
            .unwrap()
            .unwrap();
        set_account_roles(&mut c, &bob.to_string(), &["mods".into()])
            .unwrap()
            .unwrap();
        delete_role(&mut c, "mods").unwrap().unwrap();
        assert!(account_roles(&c, &bob.to_string()).unwrap().is_empty());
        assert!(!has(&c, bob, Permission::ReportsTriage).unwrap());
    }
}
