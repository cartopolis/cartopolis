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
use crate::houses;
use crate::mail::{self, Mailer};
use crate::registration::{self, Throttle};
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
    pub mailer: Arc<Mailer>,
    /// Shared by the register and re-send paths, because they cost the same
    /// things and a limit only one of them applies is not a limit.
    pub registration_throttle: Arc<RegistrationLimits>,
}

/// The two things a registration is counted against.
pub struct RegistrationLimits {
    /// Per caller. Coarse, because behind a reverse proxy a great many people
    /// share one address and this must not become a shared budget that one
    /// enthusiastic user spends for a whole university.
    pub per_caller: Throttle,
    /// Per address. Tight, because the harm this bounds is *this server mailing
    /// somebody repeatedly*, and the person being mailed did not choose the rate.
    pub per_email: Throttle,
}

impl Default for RegistrationLimits {
    fn default() -> Self {
        let hour = std::time::Duration::from_secs(3600);
        Self {
            per_caller: Throttle::new(10, hour),
            per_email: Throttle::new(3, hour),
        }
    }
}

pub fn router(state: HttpState) -> Router {
    Router::new()
        .route("/ws", get(ws_handler))
        .route("/api/health", get(health))
        .route("/api/register", get(registration_status).post(register))
        .route("/api/register/verify", post(verify_registration))
        .route("/api/register/resend", post(resend_verification))
        .route("/api/houses/search", get(search_houses))
        .route("/api/houses/claim", post(claim_house))
        .route("/api/houses/mine", get(my_house).delete(release_house))
        .route("/api/admin/login", post(login))
        .route("/api/admin/logout", post(logout))
        .route("/api/admin/me", get(me))
        .route(
            "/api/admin/accounts",
            get(list_accounts).post(create_account),
        )
        .route("/api/admin/accounts/{id}", patch(update_account))
        .route("/api/admin/accounts/{id}", delete(remove_account))
        .route("/api/admin/reports", get(list_reports))
        .route("/api/admin/reports/{id}", patch(resolve_report))
        .route("/api/admin/claims", get(list_claims))
        .route("/api/admin/claims/{id}", delete(release_claim))
        // The authoring surface is a separate module with its own note, mounted
        // here rather than defined here: it is the same audience and the same
        // token, and a second listener would double the reverse-proxy config to
        // separate two route prefixes (see this module's note).
        .merge(crate::authoring::routes())
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
    // Unauthenticated, so it is a *count* and never the reports themselves. It is
    // here because "is anybody waiting on a moderation decision?" is a question
    // an operator's existing monitoring can ask, and a queue nobody polls is how
    // "timely responses to concerns" quietly stops being true.
    let open_reports = state
        .conn
        .lock()
        .ok()
        .and_then(|c| crate::db::open_report_count(&c).ok())
        .unwrap_or(0);
    Json(json!({
        "status": "ok",
        "protocol_version": cartopolis_core::PROTOCOL_VERSION,
        "avatars": avatars,
        "open_reports": open_reports,
    }))
}

// ---------------------------------------------------------------------------
// Self-service registration
// ---------------------------------------------------------------------------

/// What the sign-up form needs to know before it draws itself: whether this
/// server takes registrations at all, and the rules it will enforce so the form
/// can enforce the same ones before spending a round trip.
///
/// Unauthenticated on purpose — it is the answer to "may I make an account
/// here?", which is a question anyone who could press the button may ask.
async fn registration_status(State(state): State<HttpState>) -> Json<serde_json::Value> {
    Json(json!({
        "open": state.config.registration_open,
        "min_password_length": MIN_PASSWORD_LEN,
        "max_username_length": MAX_USERNAME_LEN,
        // So the "check your inbox" screen can say how long the link is good for
        // without the number living in two repositories.
        "verification_valid_hours": registration::VERIFICATION_TTL.as_secs() / 3600,
    }))
}

#[derive(Deserialize)]
struct RegisterRequest {
    username: String,
    email: String,
    password: String,
}

/// Who this request is counted against for rate-limiting.
///
/// The socket's peer is the truth, except that in the deployment that matters the
/// peer is always Caddy on the docker gateway and every caller in the world looks
/// like one person. `X-Forwarded-For` is what a proxy adds — and it is also what
/// an attacker adds, so it is only believed when the peer is *itself* a private
/// address, i.e. something inside this deployment rather than the open internet.
/// A public peer that sends the header is counted by its own address regardless.
fn caller_key(headers: &HeaderMap, peer: SocketAddr) -> String {
    let trusted = match peer.ip() {
        std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local(),
    };
    if trusted {
        if let Some(forwarded) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            // The left-most entry is the original client; everything after it is
            // a proxy hop.
            .and_then(|v| v.split(',').next())
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            return forwarded.to_string();
        }
    }
    peer.ip().to_string()
}

fn too_many() -> ApiError {
    ApiError(
        StatusCode::TOO_MANY_REQUESTS,
        "too many attempts — wait a while and try again".into(),
    )
}

/// The one sentence every outcome of `POST /api/register` says.
///
/// Taken address, unknown address, mail relay having a bad afternoon: the caller
/// is told the same thing, because the alternative is a form that answers "does
/// this person have an account here?" for anyone who types an address into it.
/// The person who *owns* the address is told what actually happened, by mail.
///
/// The username is the one exception and cannot be otherwise — a name already in
/// use has to be reported or the form cannot be completed.
const CHECK_YOUR_INBOX: &str = "Check your inbox — we've sent a link to confirm that address.";

/// Create a pending account and mail a verification link.
///
/// `202 Accepted`, not `201 Created`: something was accepted for processing and
/// what happens next is up to a mailbox. A `201` here would be a claim that the
/// account exists in the sense the caller means, which it does not until the link
/// is opened.
async fn register(
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<RegisterRequest>,
) -> ApiResult<Response> {
    if !state.config.registration_open {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "this server does not take registrations".into(),
        ));
    }
    let username = body.username.trim().to_string();
    check_username(&username)?;
    check_password(&body.password)?;
    let email = registration::normalize_email(&body.email).map_err(bad_request)?;

    if !state
        .registration_throttle
        .per_caller
        .allow(&caller_key(&headers, peer))
    {
        return Err(too_many());
    }
    if !state.registration_throttle.per_email.allow(&email) {
        // Counted before anything is written, and answered with the same
        // sentence as a success — a different answer here would make the limiter
        // itself the oracle the generic answer exists to remove.
        return Ok(accepted());
    }

    // Free the usernames of registrations nobody ever confirmed before deciding
    // this one is taken. Cheap, and it is the only thing that ever runs it.
    with_db(&state, |c| registration::sweep(c).map(|_| ()))?;

    // argon2id off the runtime and with no database guard held, exactly as
    // `create_account` does it and for the same reason.
    let password = body.password.clone();
    let hash = tokio::task::spawn_blocking(move || auth::hash_password(&password))
        .await
        .map_err(|e| internal(anyhow::anyhow!("hash task: {e}")))?
        .map_err(internal)?;

    let outcome = {
        let mut guard = state.conn.lock().map_err(|_| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database unavailable".into(),
            )
        })?;
        registration::begin(&mut guard, &username, &email, &hash).map_err(internal)?
    };

    match outcome {
        registration::Begin::UsernameTaken => Err(ApiError(
            StatusCode::CONFLICT,
            format!("the name {username} is taken — pick another"),
        )),
        registration::Begin::EmailTaken { username: existing } => {
            let (subject, text) =
                mail::already_registered_message(&existing, &state.config.public_base_url);
            send_or_log(&state, &email, &subject, &text).await;
            tracing::info!("registration attempted against an address that already has an account");
            Ok(accepted())
        }
        registration::Begin::Created { principal, token } => {
            let link = verification_link(&state.config.public_base_url, &token);
            let (subject, text) = mail::verification_message(
                &username,
                &link,
                registration::VERIFICATION_TTL.as_secs() / 3600,
            );
            send_or_log(&state, &email, &subject, &text).await;
            tracing::info!(%principal, %username, "registration pending confirmation");
            Ok(accepted())
        }
    }
}

/// Confirm, and hand back a session.
///
/// Opening the link is an authentication — it proves control of the mailbox the
/// account was registered with, which is the same proof a password reset rests
/// on. Issuing a token here is what lets the very next screen ("claim your
/// house") be a signed-in one instead of a second sign-in form for a password the
/// person typed ninety seconds ago.
///
/// It is the *ordinary* session token, from the same store the game uses, and it
/// expires and revokes like any other.
fn signed_in(state: &HttpState, username: &str) -> ApiResult<serde_json::Value> {
    let principal = with_db(state, |c| auth::lookup_credential(c, username))?
        .ok_or_else(|| internal(anyhow::anyhow!("confirmed account {username} has gone")))?
        .principal;
    let token = state.sessions.issue(principal, username);
    Ok(json!({
        "username": username,
        "confirmed": true,
        "principal": principal,
        "token": token,
    }))
}

fn accepted() -> Response {
    (
        StatusCode::ACCEPTED,
        Json(json!({ "message": CHECK_YOUR_INBOX })),
    )
        .into_response()
}

/// `{base}/verify?token={token}`, whatever the base's trailing slash situation.
fn verification_link(base: &str, token: &str) -> String {
    format!("{}/verify?token={token}", base.trim_end_matches('/'))
}

/// Send, and treat a relay failure as logged-and-survived rather than as the
/// caller's problem.
///
/// The account row is already written by this point. Answering 500 would tell
/// somebody their registration failed while it is in fact sitting there holding
/// their username, and the repair — press the button again — is the one thing
/// that then cannot work. So a bounced relay becomes a warning here and a
/// "re-send" button there.
async fn send_or_log(state: &HttpState, to: &str, subject: &str, body: &str) {
    if let Err(e) = state.mailer.send(to, subject, body).await {
        tracing::warn!("could not send {subject:?}: {e:#}");
    }
}

#[derive(Deserialize)]
struct VerifyRequest {
    token: String,
}

/// Spend a verification link.
///
/// A POST and not a GET even though the link in the mail is one a browser
/// follows: the web app owns `/verify`, reads the query parameter and calls this.
/// Which matters because mail clients and corporate scanners *fetch* links to
/// check them, and a GET that mutates would have those scanners confirming
/// accounts — including, one day, an account whose mail was misdelivered.
async fn verify_registration(
    State(state): State<HttpState>,
    Json(body): Json<VerifyRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let token = body.token.trim().to_string();
    if token.is_empty() {
        return Err(bad_request("that link is missing its token"));
    }
    let outcome = {
        let mut guard = state.conn.lock().map_err(|_| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database unavailable".into(),
            )
        })?;
        registration::verify(&mut guard, &token).map_err(internal)?
    };
    match outcome {
        registration::Verify::Confirmed { username } => {
            tracing::info!(%username, "registration confirmed");
            Ok(Json(signed_in(&state, &username)?))
        }
        // The same answer as a first confirmation. It is the truthful one: the
        // account is confirmed, which is what was asked for.
        registration::Verify::AlreadyConfirmed { username } => {
            Ok(Json(signed_in(&state, &username)?))
        }
        registration::Verify::Expired => Err(ApiError(
            StatusCode::GONE,
            "that link has expired — ask for a new one".into(),
        )),
        registration::Verify::Unknown => Err(ApiError(
            StatusCode::NOT_FOUND,
            "that link is not valid — it may have been used already, or replaced by a newer one"
                .into(),
        )),
    }
}

#[derive(Deserialize)]
struct ResendRequest {
    email: String,
}

/// Mail a fresh link to a pending account.
///
/// Answers 202 whatever it finds, for the reason [`CHECK_YOUR_INBOX`] gives:
/// unknown address, already-confirmed account and successful re-send are one
/// response, or this endpoint becomes the account oracle the register endpoint
/// refuses to be.
async fn resend_verification(
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<ResendRequest>,
) -> ApiResult<Response> {
    let Ok(email) = registration::normalize_email(&body.email) else {
        return Ok(accepted());
    };
    if !state
        .registration_throttle
        .per_caller
        .allow(&caller_key(&headers, peer))
    {
        return Err(too_many());
    }
    if !state.registration_throttle.per_email.allow(&email) {
        return Ok(accepted());
    }

    let reissued = with_db(&state, |c| {
        let Some(pending) = registration::pending_by_email(c, &email)? else {
            return Ok(None);
        };
        let token = registration::reissue(c, &pending)?;
        Ok(Some((pending.username, token)))
    })?;

    if let Some((username, token)) = reissued {
        let link = verification_link(&state.config.public_base_url, &token);
        let (subject, text) = mail::verification_message(
            &username,
            &link,
            registration::VERIFICATION_TTL.as_secs() / 3600,
        );
        send_or_log(&state, &email, &subject, &text).await;
        tracing::info!(%username, "verification link re-sent");
    }
    Ok(accepted())
}

// ---------------------------------------------------------------------------
// Admin API
// ---------------------------------------------------------------------------

/// An error as the API reports it: a status and a message a GUI can show.
pub(crate) struct ApiError(pub StatusCode, pub String);

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

pub(crate) type ApiResult<T> = Result<T, ApiError>;

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
pub(crate) fn require_admin(state: &HttpState, headers: &HeaderMap) -> ApiResult<(Uuid, String)> {
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

/// Whether a principal is an enabled admin *right now*.
///
/// Split out of [`require_admin`] because two routes need the answer without
/// requiring it: `/api/content/export` and `/api/content/transfer` are open to any
/// signed-in account and merely reach *further* for an admin, so they ask rather
/// than demand.
pub(crate) fn is_caller_admin(state: &HttpState, principal: Uuid) -> ApiResult<bool> {
    with_db(state, |c| auth::is_admin(c, principal))
}

/// Run a closure with the SQLite connection, mapping a poisoned lock to a 500.
///
/// Every admin handler goes through this rather than locking inline, because the
/// rule it enforces is easy to break by accident and expensive when broken: the
/// guard is taken, used and dropped inside one non-async closure, so it can never
/// be held across an `.await`. The tick loop takes the same mutex while holding
/// the world write lock (see `auth`'s module note), so an admin request that
/// parked with it held would stall the simulation for everyone.
pub(crate) fn with_db<T>(
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
    let stored = with_db(&state, |c| auth::lookup_credential(c, &body.username))?;
    let unauthorized = || {
        ApiError(
            StatusCode::UNAUTHORIZED,
            "wrong username or password".into(),
        )
    };
    let stored = stored.ok_or_else(unauthorized)?;
    let principal = stored.principal;
    if !auth::verify_password_bounded(body.password, stored.phc).await {
        return Err(unauthorized());
    }
    // The password is proved before the account's state is mentioned, for the
    // reason `net::authenticate` documents. It cannot arise here in practice — a
    // registration is never an admin — but the rule is about the order, and a
    // rule with an exception in it is not one.
    if stored.email_pending {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "confirm your email address first — check your inbox for the link".into(),
        ));
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
    // Same erasure the in-app "delete my account" performs — an account deleted
    // by an operator must not leave more of a person behind than one deleted by
    // its owner, or the guarantee is a property of which button was pressed.
    with_db(&state, |c| {
        crate::db::erase_account_traces(c, &id, &target.username)
    })?;
    with_db(&state, |c| auth::delete_account(c, &id))?;
    // The row is gone, so any token still resolving to it authenticates as
    // nobody. `set_disabled` can leave a live session standing (it gates the next
    // handshake); deletion cannot.
    let revoked = state
        .sessions
        .revoke_principal(Uuid::parse_str(&id).unwrap_or_else(|_| Uuid::nil()));
    tracing::info!(%caller, target = %id, revoked, "account deleted");
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Claiming a house
// ---------------------------------------------------------------------------

/// The caller's principal, if they presented a live token. **No admin check** —
/// this is a player acting on their own account.
///
/// The distinction from [`require_admin`] is the whole security model of these
/// three routes: everything they do is scoped to the principal this returns, so
/// there is no id in any request body that could name somebody else's claim.
pub(crate) fn require_session(state: &HttpState, headers: &HeaderMap) -> ApiResult<(Uuid, String)> {
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
    state.sessions.resolve(token).ok_or(ApiError(
        StatusCode::UNAUTHORIZED,
        "that session has expired".to_string(),
    ))
}

fn claims_open(state: &HttpState) -> ApiResult<()> {
    if state.config.house_claims_open {
        return Ok(());
    }
    Err(ApiError(
        StatusCode::FORBIDDEN,
        "this server does not hand out houses".into(),
    ))
}

#[derive(serde::Deserialize)]
struct HouseQuery {
    q: String,
}

/// Addresses matching what the person typed, each marked available or not.
///
/// Reaches two services somebody else runs, so it is throttled per caller on the
/// same limiter the registration paths use: a search box that fires per keystroke
/// against a public register is how a keyless service stops being keyless.
async fn search_houses(
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<HouseQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    require_session(&state, &headers)?;
    claims_open(&state)?;
    if !state
        .registration_throttle
        .per_caller
        .allow(&format!("search:{}", caller_key(&headers, peer)))
    {
        return Err(too_many());
    }

    let municipality = state.config.claim_municipality.clone();
    let q = query.q.clone();
    let found = tokio::task::spawn_blocking(move || houses::search(&q, &municipality))
        .await
        .map_err(|e| internal(anyhow::anyhow!("address search task: {e}")))?
        .map_err(|e| {
            tracing::warn!("address search: {e:#}");
            ApiError(
                StatusCode::BAD_GATEWAY,
                "the address register did not answer — try again in a moment".into(),
            )
        })?;

    let ids: Vec<String> = found
        .iter()
        .map(|a| a.nummeraanduiding_id.clone())
        .collect();
    let taken = with_db(&state, |c| houses::taken(c, &ids))?;
    let results: Vec<serde_json::Value> = found
        .into_iter()
        .map(|a| {
            let available = !taken.contains(&a.nummeraanduiding_id);
            let mut value = serde_json::to_value(&a).unwrap_or_else(|_| json!({}));
            if let Some(object) = value.as_object_mut() {
                object.insert("available".into(), json!(available));
            }
            value
        })
        .collect();
    Ok(Json(
        json!({ "municipality": state.config.claim_municipality, "results": results }),
    ))
}

#[derive(Deserialize)]
struct ClaimRequest {
    nummeraanduiding_id: String,
}

/// Claim one address for the signed-in account.
///
/// The id from the search is treated as a *hint*, not as a fact: the address is
/// re-resolved from the register here, and the municipality re-checked against
/// the server's own setting. Otherwise the id is just a field, and a claim in
/// another province is one `curl` away from a rule that only the search box
/// enforces.
async fn claim_house(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<ClaimRequest>,
) -> ApiResult<Response> {
    let (principal, username) = require_session(&state, &headers)?;
    claims_open(&state)?;

    let municipality = state.config.claim_municipality.clone();
    let id = body.nummeraanduiding_id.trim().to_string();
    let resolved = {
        let id = id.clone();
        let municipality = municipality.clone();
        tokio::task::spawn_blocking(move || houses::lookup(&id, &municipality))
            .await
            .map_err(|e| internal(anyhow::anyhow!("address lookup task: {e}")))?
            .map_err(|e| {
                tracing::warn!("address lookup: {e:#}");
                ApiError(
                    StatusCode::BAD_GATEWAY,
                    "the address register did not answer — try again in a moment".into(),
                )
            })?
    };
    let address = resolved.ok_or_else(|| {
        ApiError(
            StatusCode::NOT_FOUND,
            format!("that address is not one of {municipality}'s"),
        )
    })?;

    let object_id = address.adresseerbaarobject_id.clone();
    let pand = tokio::task::spawn_blocking(move || houses::pand_for(&object_id))
        .await
        .map_err(|e| internal(anyhow::anyhow!("building lookup task: {e}")))?
        .map_err(|e| {
            tracing::warn!("building lookup: {e:#}");
            ApiError(
                StatusCode::BAD_GATEWAY,
                "the building register did not answer — try again in a moment".into(),
            )
        })?;
    // A berth or a pitch: a real address with real people at it and no building
    // in 3DBAG, so there is nothing on the map to hand over.
    let pand = pand.ok_or_else(|| {
        ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "that address has no building in the register — a houseboat berth or a pitch".into(),
        )
    })?;

    let outcome = with_db(&state, |c| houses::claim(c, principal, &address, &pand))?;
    match outcome {
        houses::Claimed::Ok => {
            tracing::info!(%username, address = %address.address, pand = %pand, "house claimed");
            let claim = with_db(&state, |c| houses::for_principal(c, principal))?;
            Ok((StatusCode::CREATED, Json(json!({ "claim": claim }))).into_response())
        }
        houses::Claimed::AddressTaken => Err(ApiError(
            StatusCode::CONFLICT,
            "somebody already has that house".into(),
        )),
        houses::Claimed::AlreadyHolding { address } => Err(ApiError(
            StatusCode::CONFLICT,
            format!("you already have {address} — give it up first"),
        )),
    }
}

async fn my_house(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let (principal, _) = require_session(&state, &headers)?;
    let claim = with_db(&state, |c| houses::for_principal(c, principal))?;
    Ok(Json(json!({ "claim": claim })))
}

async fn release_house(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<StatusCode> {
    let (principal, username) = require_session(&state, &headers)?;
    let released = with_db(&state, |c| houses::release(c, principal))?;
    if released {
        tracing::info!(%username, "house released");
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Every claim, for the operator. The dispute-resolution surface: claiming is
/// first come first served, so the only answer to "that is my house" is a person
/// with this list and the button below it.
async fn list_claims(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&state, &headers)?;
    let claims = with_db(&state, houses::list)?;
    Ok(Json(json!({ "claims": claims })))
}

async fn release_claim(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<StatusCode> {
    let (caller, _) = require_admin(&state, &headers)?;
    if !with_db(&state, |c| houses::release_address(c, &id))? {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such claim".into()));
    }
    tracing::info!(%caller, address = %id, "house claim released by an operator");
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Reports (App Store guideline 1.2: "a mechanism to report offensive content
// and timely responses to concerns")
// ---------------------------------------------------------------------------

/// The operator's queue. `?all=true` includes resolved reports.
///
/// On the admin API rather than the wire for the same reason account management
/// is: this is an authorisation decision about other people, and putting it on
/// `NetMessage` would move every client's protocol version whenever the operator
/// surface changed. The *filing* half is on the wire, because that is a player
/// action.
async fn list_reports(
    State(state): State<HttpState>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<ReportQuery>,
) -> ApiResult<Json<Vec<crate::db::ReportRow>>> {
    require_admin(&state, &headers)?;
    let rows = with_db(&state, |c| {
        crate::db::list_reports(c, !q.all, q.limit.unwrap_or(200))
    })?;
    Ok(Json(rows))
}

#[derive(serde::Deserialize, Default)]
struct ReportQuery {
    #[serde(default)]
    all: bool,
    limit: Option<i64>,
}

#[derive(serde::Deserialize)]
struct ResolveReport {
    /// What was done about it. Free text, and stored verbatim: the queue is only
    /// useful if the answer to "was this handled?" is legible a month later.
    #[serde(default)]
    resolution: String,
}

async fn resolve_report(
    State(state): State<HttpState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Json(body): Json<ResolveReport>,
) -> ApiResult<StatusCode> {
    let (caller, _) = require_admin(&state, &headers)?;
    let ok = with_db(&state, |c| {
        crate::db::resolve_report(c, id, &body.resolution)
    })?;
    if !ok {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such report".into()));
    }
    tracing::info!(%caller, report_id = id, "report resolved");
    Ok(StatusCode::NO_CONTENT)
}
