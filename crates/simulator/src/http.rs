//! The simulator's HTTP surface: the browser's game transport and the account
//! admin API, on one listener.
//!
//! # Why one listener and not two
//!
//! Both are things a *browser* talks to, both need the same TLS termination, and
//! Caddy fronts one upstream per hostname. Splitting them would double the reverse
//! proxy configuration to separate two route prefixes that are already separated
//! by being route prefixes.
//!
//! # Why HTTP at all
//!
//! The wire (`cartopolis_core::protocol`) has no account-management messages and
//! is not the place for them: they are operator actions, not gameplay, and adding
//! them would put an authorisation decision on the same code path as a movement
//! intent. The admin API is a separate surface with a separate audience, and the
//! only thing it shares with the game is the account store and the session map —
//! which is the point of sharing them, since an admin signs in with an ordinary
//! account and disabling someone takes effect on their next handshake.
//!
//! # The listener is opt-in
//!
//! `SimConfig::http_listen` defaults to `None`, so nothing about solo mode or the
//! existing tests changes: they get the TCP listener they have always had and no
//! HTTP port at all. A deployment turns it on.

use crate::auth::{self, Sessions};
use crate::config::SimConfig;
use crate::scripting::ScriptHandle;
use crate::state::SimWorld;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use bytes::{Bytes, BytesMut};
use futures_util::{Sink, Stream};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::sync::{broadcast, RwLock};
use uuid::Uuid;

/// Everything a request handler may need. Cloned per request; every field is
/// already an `Arc` or a channel handle, so this is pointer copies.
#[derive(Clone)]
pub struct HttpState {
    pub world: Arc<RwLock<SimWorld>>,
    pub config: Arc<SimConfig>,
    pub tx_snap: broadcast::Sender<Vec<u8>>,
    pub conn: Arc<Mutex<rusqlite::Connection>>,
    pub sessions: Arc<Sessions>,
    pub chat: Arc<crate::chat::ChatHub>,
    pub scripts: ScriptHandle,
}

pub fn router(state: HttpState) -> Router {
    Router::new()
        .route("/ws", get(ws_handler))
        .route("/api/health", get(health))
        .route("/api/admin/login", post(login))
        .route("/api/admin/logout", post(logout))
        .route("/api/admin/me", get(me))
        .route(
            "/api/admin/accounts",
            get(list_accounts).post(create_account),
        )
        .route("/api/admin/accounts/{id}", patch(update_account))
        .route("/api/admin/accounts/{id}", delete(remove_account))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// The browser's game transport
// ---------------------------------------------------------------------------

/// One WebSocket message per app frame, adapting axum's `WebSocket` to the byte
/// boundary [`crate::net::FrameTransport`] is written against.
///
/// Non-binary messages are **skipped, not errors**. A browser stack (and any
/// proxy between it and here) may send a ping, a pong or a close at any time, and
/// treating one as a protocol violation would drop a healthy session because
/// something in the middle wanted a keepalive. `Close` ends the stream, which is
/// what the loop above reads as a clean disconnect.
struct WsFrames {
    inner: WebSocket,
}

impl Stream for WsFrames {
    type Item = std::io::Result<BytesMut>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            return match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Ready(Some(Err(e))) => {
                    Poll::Ready(Some(Err(std::io::Error::other(e.to_string()))))
                }
                Poll::Ready(Some(Ok(Message::Binary(bytes)))) => {
                    Poll::Ready(Some(Ok(BytesMut::from(&bytes[..]))))
                }
                Poll::Ready(Some(Ok(Message::Close(_)))) => Poll::Ready(None),
                // Text/Ping/Pong: not ours. Poll again rather than returning
                // Pending, which would park this task with no waker registered.
                Poll::Ready(Some(Ok(_))) => continue,
            };
        }
    }
}

impl Sink<Bytes> for WsFrames {
    type Error = std::io::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner)
            .poll_ready(cx)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    fn start_send(mut self: Pin<&mut Self>, item: Bytes) -> Result<(), Self::Error> {
        Pin::new(&mut self.inner)
            .start_send(Message::Binary(item))
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner)
            .poll_flush(cx)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner)
            .poll_close(cx)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<HttpState>,
) -> Response {
    // The same 32 MiB ceiling the TCP codec enforces (`net::MAX_FRAME`). Without
    // it the WS carrier would be the more permissive of the two, and a texture
    // upload that the TCP client cannot make would be one a browser client can.
    ws.max_message_size(crate::net::MAX_FRAME)
        .on_upgrade(move |socket| async move {
            tracing::info!(%addr, "accepted (websocket)");
            let framed = WsFrames { inner: socket };
            if let Err(e) = crate::net::handle_connection(
                framed,
                state.world,
                state.config,
                state.tx_snap.subscribe(),
                state.tx_snap.clone(),
                state.conn,
                state.sessions,
                state.chat,
                state.scripts,
            )
            .await
            {
                tracing::warn!(%addr, "client ended: {e:#}");
            }
        })
}

async fn health(State(state): State<HttpState>) -> Json<serde_json::Value> {
    let avatars = state.world.read().await.avatar_count();
    Json(json!({
        "status": "ok",
        "protocol_version": cartopolis_core::PROTOCOL_VERSION,
        "avatars": avatars,
    }))
}

// ---------------------------------------------------------------------------
// Admin API
// ---------------------------------------------------------------------------

/// An error as the API reports it: a status and a message a GUI can show.
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn bad_request(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

fn internal(e: anyhow::Error) -> ApiError {
    // Logged in full, reported in outline: the chain names table and column
    // names, and the caller is a browser.
    tracing::warn!("admin api: {e:#}");
    ApiError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "the server could not complete that".into(),
    )
}

type ApiResult<T> = Result<T, ApiError>;

/// Shortest password the API will store.
///
/// Not a policy so much as a floor: argon2id makes a *guessed* password expensive
/// to test, and does nothing at all about a three-character one.
const MIN_PASSWORD_LEN: usize = 8;
const MAX_USERNAME_LEN: usize = 64;

fn check_password(password: &str) -> ApiResult<()> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(bad_request(format!(
            "password must be at least {MIN_PASSWORD_LEN} characters"
        )));
    }
    Ok(())
}

fn check_username(username: &str) -> ApiResult<()> {
    let n = username.chars().count();
    if n == 0 || n > MAX_USERNAME_LEN {
        return Err(bad_request(format!(
            "username must be 1-{MAX_USERNAME_LEN} characters"
        )));
    }
    if username.trim() != username {
        return Err(bad_request("username may not begin or end with whitespace"));
    }
    Ok(())
}

/// The caller's principal, if they presented a live token *and* the account
/// behind it is an enabled admin right now.
///
/// Every admin route begins here. Both halves matter: the token proves who, and
/// the database says whether that person may still do this — so revoking the flag
/// or disabling the account takes effect on the next request rather than at token
/// expiry.
fn require_admin(state: &HttpState, headers: &HeaderMap) -> ApiResult<(Uuid, String)> {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or(ApiError(
            StatusCode::UNAUTHORIZED,
            "sign in first".to_string(),
        ))?;
    let (principal, username) = state.sessions.resolve(token).ok_or(ApiError(
        StatusCode::UNAUTHORIZED,
        "that session has expired".to_string(),
    ))?;
    let admin = {
        let guard = state.conn.lock().map_err(|_| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database unavailable".into(),
            )
        })?;
        auth::is_admin(&guard, principal).map_err(internal)?
    };
    if !admin {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "that account may not administer this server".into(),
        ));
    }
    Ok((principal, username))
}

/// Run a closure with the SQLite connection, mapping a poisoned lock to a 500.
///
/// Every admin handler goes through this rather than locking inline, because the
/// rule it enforces is easy to break by accident and expensive when broken: the
/// guard is taken, used and dropped inside one non-async closure, so it can never
/// be held across an `.await`. The tick loop takes the same mutex while holding
/// the world write lock (see `auth`'s module note), so an admin request that
/// parked with it held would stall the simulation for everyone.
fn with_db<T>(
    state: &HttpState,
    f: impl FnOnce(&rusqlite::Connection) -> anyhow::Result<T>,
) -> ApiResult<T> {
    let guard = state.conn.lock().map_err(|_| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database unavailable".into(),
        )
    })?;
    f(&guard).map_err(internal)
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

/// Sign in to the admin API. Issues a token from the *same* session store the
/// game uses, so there is one notion of "signed in" per account.
async fn login(
    State(state): State<HttpState>,
    Json(body): Json<LoginRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    // Read the row, drop the guard, then hash — the module note in `auth` explains
    // why an argon2 verify must never happen with this mutex held.
    let stored = with_db(&state, |c| auth::lookup_account(c, &body.username))?;
    let unauthorized = || {
        ApiError(
            StatusCode::UNAUTHORIZED,
            "wrong username or password".into(),
        )
    };
    let (principal, phc) = stored.ok_or_else(unauthorized)?;
    if !auth::verify_password_bounded(body.password, phc).await {
        return Err(unauthorized());
    }
    // Authenticated, but the API is admin-only: a player's correct password gets
    // the same answer as a wrong one, so this endpoint cannot be used to tell
    // which usernames exist.
    if !with_db(&state, |c| auth::is_admin(c, principal))? {
        return Err(unauthorized());
    }
    let token = state.sessions.issue(principal, &body.username);
    Ok(Json(json!({
        "token": token,
        "principal": principal,
        "username": body.username,
    })))
}

async fn logout(State(state): State<HttpState>, headers: HeaderMap) -> StatusCode {
    if let Some(token) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        state.sessions.revoke(token.trim());
    }
    StatusCode::NO_CONTENT
}

async fn me(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let (principal, username) = require_admin(&state, &headers)?;
    Ok(Json(json!({
        "principal": principal,
        "username": username,
        "admin": true,
    })))
}

async fn list_accounts(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&state, &headers)?;
    let accounts = with_db(&state, auth::list_accounts)?;
    Ok(Json(json!({ "accounts": accounts })))
}

#[derive(Deserialize)]
struct CreateRequest {
    username: String,
    password: String,
    #[serde(default)]
    admin: bool,
}

async fn create_account(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<CreateRequest>,
) -> ApiResult<Response> {
    require_admin(&state, &headers)?;
    check_username(&body.username)?;
    check_password(&body.password)?;

    // argon2id is ~20 ms and ~19 MiB by design. Off the runtime, and — as ever —
    // with no database guard held.
    let password = body.password.clone();
    let hash = tokio::task::spawn_blocking(move || auth::hash_password(&password))
        .await
        .map_err(|e| internal(anyhow::anyhow!("hash task: {e}")))?
        .map_err(internal)?;

    let id = Uuid::new_v4();
    let created = with_db(&state, |c| {
        let n = c.execute(
            "INSERT OR IGNORE INTO accounts (id, username, pw_hash, admin) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id.to_string(), body.username, hash, i64::from(body.admin)],
        )?;
        Ok(n > 0)
    })?;
    if !created {
        return Err(ApiError(
            StatusCode::CONFLICT,
            format!("an account named {} already exists", body.username),
        ));
    }
    let account = with_db(&state, |c| auth::get_account(c, &id.to_string()))?;
    tracing::info!(username = %body.username, admin = body.admin, "account created");
    Ok((StatusCode::CREATED, Json(json!({ "account": account }))).into_response())
}

#[derive(Deserialize)]
struct UpdateRequest {
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    disabled: Option<bool>,
    #[serde(default)]
    admin: Option<bool>,
}

async fn update_account(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<UpdateRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let (caller, _) = require_admin(&state, &headers)?;
    let target = with_db(&state, |c| auth::get_account(c, &id))?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "no such account".into()))?;

    // Refuse anything that would remove the last way in. Checked before the write
    // rather than repaired after it — there is no repair from a server with no
    // enabled admin except a shell on the box.
    let losing_admin = body.admin == Some(false) || body.disabled == Some(true);
    if target.admin && !target.disabled && losing_admin {
        let admins = with_db(&state, auth::enabled_admin_count)?;
        if admins <= 1 {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "that is the last enabled admin — promote someone else first".into(),
            ));
        }
    }

    if let Some(password) = body.password {
        check_password(&password)?;
        let hash = tokio::task::spawn_blocking(move || auth::hash_password(&password))
            .await
            .map_err(|e| internal(anyhow::anyhow!("hash task: {e}")))?
            .map_err(internal)?;
        with_db(&state, |c| {
            c.execute(
                "UPDATE accounts SET pw_hash = ?2 WHERE id = ?1",
                rusqlite::params![id, hash],
            )?;
            Ok(())
        })?;
    }
    if let Some(disabled) = body.disabled {
        with_db(&state, |c| auth::set_disabled(c, &id, disabled))?;
    }
    if let Some(admin) = body.admin {
        with_db(&state, |c| auth::set_admin(c, &id, admin))?;
    }

    let account = with_db(&state, |c| auth::get_account(c, &id))?;
    tracing::info!(%caller, target = %id, "account updated");
    Ok(Json(json!({ "account": account })))
}

async fn remove_account(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<StatusCode> {
    let (caller, _) = require_admin(&state, &headers)?;
    if caller.to_string() == id {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "you cannot delete the account you are signed in with".into(),
        ));
    }
    let target = with_db(&state, |c| auth::get_account(c, &id))?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "no such account".into()))?;
    if target.admin && !target.disabled {
        let admins = with_db(&state, auth::enabled_admin_count)?;
        if admins <= 1 {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "that is the last enabled admin — promote someone else first".into(),
            ));
        }
    }
    with_db(&state, |c| auth::delete_account(c, &id))?;
    tracing::info!(%caller, target = %id, "account deleted");
    Ok(StatusCode::NO_CONTENT)
}
