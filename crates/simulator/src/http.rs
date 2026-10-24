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
use crate::buildings;
use crate::config::SimConfig;
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
use cartopolis_core::permissions::{Permission, PermissionSet};
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
    pub tx_snap: broadcast::Sender<bytes::Bytes>,
    pub conn: Arc<Mutex<rusqlite::Connection>>,
    pub sessions: Arc<Sessions>,
    pub chat: Arc<crate::chat::ChatHub>,
    pub scripts: ScriptHandle,
    pub calls: Option<crate::calls::CallsHandle>,
    /// The key browsers subscribe to call pushes with (`crate::vapid`); `None`
    /// on a server without calls.
    pub vapid: Option<Arc<crate::vapid::Vapid>>,
    /// Prim create/edit/delete/script, shared with the wire so the two surfaces
    /// cannot disagree about who may do what (`crate::prims`).
    pub prims: crate::prims::PrimService,
    pub mailer: Arc<Mailer>,
    /// Shared by the register and re-send paths, because they cost the same
    /// things and a limit only one of them applies is not a limit.
    pub registration_throttle: Arc<RegistrationLimits>,
    /// Public posts and the fediverse (`crate::fediverse`).
    pub fedi: crate::fediverse::Federation,
    /// Usage counts (`crate::stats`).
    pub stats: Arc<crate::stats::Stats>,
    /// Page views per caller: generous, since a whole school can share one
    /// address, but a script cannot inflate the counts without end.
    pub stats_throttle: Arc<Throttle>,
    /// Signing in with Discord, Google and the rest: round trips in flight,
    /// their answers, the app's pairing codes and the key provider secrets are
    /// sealed with (`crate::signin`).
    pub signin: Arc<crate::signin::SignIn>,
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
        .route("/api/buildings/search", get(search_buildings))
        .route("/api/buildings/claim", post(claim_building))
        .route("/api/buildings/mine", get(my_buildings))
        .route("/api/buildings/{pand}", delete(release_building))
        .route("/api/buildings/{pand}/access", get(building_access))
        .route("/api/push/key", get(push_key))
        .route("/api/login", post(player_login))
        .route("/api/logout", post(logout))
        .route("/api/tokens", get(list_tokens).post(create_token))
        .route("/api/tokens/{id}", delete(revoke_token))
        .route("/api/admin/login", post(login))
        .route("/api/admin/logout", post(logout))
        .route("/api/admin/me", get(me))
        .route(
            "/api/admin/accounts",
            get(list_accounts).post(create_account),
        )
        .route("/api/admin/accounts/{id}", patch(update_account))
        .route("/api/admin/accounts/{id}/activate", post(activate_account))
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
        // Signing in with Discord, Google and the rest, the account's own list
        // of them, and Admin › Sign-in (`docs/notes/external-sign-in.md`).
        .merge(crate::signin::http::routes())
        // Model assets, inventories and the shared library. Mounted here for the
        // same reason the authoring surface is: one listener, one token, one
        // reverse-proxy prefix to configure.
        .merge(crate::library::routes())
        // Roles and who holds them (`docs/notes/permissions.md`).
        .merge(crate::roles::routes())
        .merge(crate::videos::routes())
        .merge(crate::pictures::routes())
        // Communities' public posts, and what other fediverse servers ask
        // for (`crate::fediverse`).
        .merge(crate::fediverse::routes::routes())
        // Usage statistics: the page-view beacon, Admin › Stats and a
        // community's own numbers (`crate::stats`).
        .merge(crate::stats::api::routes())
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
                    // The frame's own buffer when nothing else holds it, which is
                    // the usual case; a copy only when it is shared.
                    Poll::Ready(Some(Ok(bytes
                        .try_into_mut()
                        .unwrap_or_else(|b| BytesMut::from(&b[..])))))
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

/// What an app says about itself on the `/ws` address
/// (`/ws?client=android&build=410d46f4`), for `crate::stats`. On the address
/// rather than in `ClientHello` because a field there is a protocol revision
/// every installed app would have to take; an app that sends neither is
/// counted as `unknown`.
#[derive(Deserialize, Default)]
struct WsQuery {
    client: Option<String>,
    build: Option<String>,
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<HttpState>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<WsQuery>,
) -> Response {
    let arrival = crate::stats::Arrival {
        stats: state.stats.clone(),
        client: crate::stats::client_tag(q.client.as_deref()),
        build: crate::stats::build_tag(q.build.as_deref()),
        ip: caller_key(&headers, addr),
        user_agent: headers
            .get(axum::http::header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string(),
    };
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
                state.conn,
                state.sessions,
                state.chat,
                state.scripts,
                state.calls,
                state.prims,
                arrival,
            )
            .await
            {
                tracing::warn!(%addr, "client ended: {e:#}");
            }
        })
}

/// The public half of this server's web push key, which a browser subscribes
/// with (`crate::vapid`). Unauthenticated: it is a public key, and the page
/// asks for it before it knows it will be allowed to use it.
async fn push_key(State(state): State<HttpState>) -> Response {
    match &state.vapid {
        Some(vapid) => Json(json!({ "key": vapid.public_key() })).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "this server rings no calls" })),
        )
            .into_response(),
    }
}

async fn health(State(state): State<HttpState>) -> Json<serde_json::Value> {
    let avatars = state.world.read().await.avatar_count();
    // Unauthenticated, so it is a *count* and never the reports themselves. It is
    // here because "is anybody waiting on a moderation decision?" is a question
    // an operator's existing monitoring can ask, and a queue nobody polls is how
    // "timely responses to concerns" quietly stops being true.
    let open_reports = crate::db::blocking(|| {
        state
            .conn
            .lock()
            .ok()
            .and_then(|c| crate::db::open_report_count(&c).ok())
            .unwrap_or(0)
    });
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
    // The buttons the sign-in card draws above the password form, in the
    // operator's order. A database that cannot answer draws none rather than
    // failing the whole form.
    let providers = with_db(&state, crate::signin::store::enabled_providers)
        .map(|list| {
            list.into_iter()
                .map(|p| json!({ "id": p.id, "kind": p.kind.as_str(), "name": p.name }))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Json(json!({
        "providers": providers,
        "open": state.config.registration_open,
        "min_password_length": MIN_PASSWORD_LEN,
        "max_username_length": MAX_USERNAME_LEN,
        // So the "check your inbox" screen can say how long the link is good for
        // without the number living in two repositories.
        "verification_valid_hours": registration::VERIFICATION_TTL.as_secs() / 3600,
        // Read by the sign-in screen to decide whether to offer the way to the
        // claim-a-building page at all — a link on a server that does not hand
        // out buildings would lead to a refusal.
        "houses_open": state.config.house_claims_open,
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
pub(crate) fn caller_key(headers: &HeaderMap, peer: SocketAddr) -> String {
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

    let outcome = with_db_mut(&state, |c| registration::begin(c, &username, &email, &hash))?;

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
    let outcome = with_db_mut(&state, |c| registration::verify(c, &token))?;
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

/// The refusal an account without a password gets where others are asked for
/// theirs again. Answered with `"reauth": true` beside the sentence, so the web
/// UI can offer the button that signs in through Discord, Google or the rest
/// once more (`signin`) instead of a password field nobody can fill in.
pub(crate) const REAUTH_NEEDED: &str =
    "confirm it's you first: sign in again with the account you use here";

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({ "error": self.1 });
        if self.1 == REAUTH_NEEDED {
            body["reauth"] = json!(true);
        }
        (self.0, Json(body)).into_response()
    }
}

pub(crate) fn bad_request(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

pub(crate) fn internal(e: anyhow::Error) -> ApiError {
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

pub(crate) fn check_password(password: &str) -> ApiResult<()> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(bad_request(format!(
            "password must be at least {MIN_PASSWORD_LEN} characters"
        )));
    }
    Ok(())
}

pub(crate) fn check_username(username: &str) -> ApiResult<()> {
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

/// Whether a principal is an enabled admin *right now* — the flag itself, which
/// only matters now for who may make or unmake an admin (`admin_only`) and for
/// what the web UI shows; every other decision is a permission ([`require`]).
pub(crate) fn is_caller_admin(state: &HttpState, principal: Uuid) -> ApiResult<bool> {
    with_db(state, |c| auth::is_admin(c, principal))
}

/// [`is_caller_admin`] for a credential: an access token made without the
/// elevated permissions is not an admin, whatever the account is.
pub(crate) fn is_access_admin(state: &HttpState, access: &auth::Access) -> ApiResult<bool> {
    if access.permissions(PermissionSet::all()) != PermissionSet::all() {
        return Ok(false);
    }
    is_caller_admin(state, access.principal)
}

/// What a principal may do *right now* (`roles::effective`).
///
/// Read per request rather than carried in the session: the token proves who,
/// and the database says what that person may still do — so taking a role away
/// or disabling the account takes effect on the next request rather than at
/// token expiry.
pub(crate) fn caller_permissions(state: &HttpState, principal: Uuid) -> ApiResult<PermissionSet> {
    with_db(state, |c| crate::roles::effective(c, principal))
}

/// What a presented credential may do right now: the account's permissions,
/// less the elevated ones when it is a restricted access token
/// (`auth::Access::permissions`). Every route that decides on a permission asks
/// this rather than [`caller_permissions`], so a bot's token cannot run the
/// server just because its account can.
pub(crate) fn access_permissions(
    state: &HttpState,
    access: &auth::Access,
) -> ApiResult<PermissionSet> {
    Ok(access.permissions(caller_permissions(state, access.principal)?))
}

/// The caller's principal, if they presented a live token and their roles hold
/// `p` right now (`docs/notes/permissions.md`). Every operator route begins
/// here; the refusal names what was missing.
pub(crate) fn require(
    state: &HttpState,
    headers: &HeaderMap,
    p: Permission,
) -> ApiResult<(Uuid, String)> {
    let access = require_access(state, headers)?;
    if !access_permissions(state, &access)?.has(p) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            format!(
                "that account may not do this here ({}: {})",
                p.slug(),
                p.label().to_lowercase()
            ),
        ));
    }
    Ok((access.principal, access.username))
}

/// Whether an account has anything to do in the operator web UI: a permission
/// from the server group, or the building override. Who may sign in there.
#[must_use]
pub(crate) fn is_operator(perms: PermissionSet) -> bool {
    perms.iter().any(|p| {
        p.group() == cartopolis_core::permissions::PermissionGroup::Server
            || p == Permission::BuildingOverride
            || p == Permission::BuildingDesignAny
    })
}

/// Run a closure with the SQLite connection, mapping a poisoned lock to a 500.
///
/// Every admin handler goes through this rather than locking inline, because the
/// rule it enforces is easy to break by accident and expensive when broken: the
/// guard is taken, used and dropped inside one non-async closure, so it can never
/// be held across an `.await`. And it runs through [`crate::db::blocking`], so
/// the worker thread this request is on is handed back to the runtime for the
/// length of the query rather than parked on the disk.
pub(crate) fn with_db<T>(
    state: &HttpState,
    f: impl FnOnce(&rusqlite::Connection) -> anyhow::Result<T>,
) -> ApiResult<T> {
    with_db_mut(state, |c| f(&*c))
}

/// [`with_db`] for a body that needs the connection mutably — a transaction.
pub(crate) fn with_db_mut<T>(
    state: &HttpState,
    f: impl FnOnce(&mut rusqlite::Connection) -> anyhow::Result<T>,
) -> ApiResult<T> {
    crate::db::blocking(|| {
        let mut guard = state.conn.lock().map_err(|_| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database unavailable".into(),
            )
        })?;
        f(&mut *guard).map_err(internal)
    })
}

#[derive(Deserialize)]
pub(crate) struct LoginRequest {
    pub(crate) username: String,
    pub(crate) password: String,
}

/// The password check both login routes share.
///
/// `lookup_credential` already filters `disabled = 1`, so a disabled account
/// gets the same answer as an unknown one.
pub(crate) async fn check_password_login(state: &HttpState, body: &LoginRequest) -> ApiResult<Uuid> {
    // Read the row, drop the guard, then hash — the module note in `auth` explains
    // why an argon2 verify must never happen with this mutex held.
    let stored = with_db(state, |c| auth::lookup_credential(c, &body.username))?;
    let unauthorized = || {
        ApiError(
            StatusCode::UNAUTHORIZED,
            "wrong username or password".into(),
        )
    };
    let stored = stored.ok_or_else(unauthorized)?;
    if !auth::verify_password_bounded(body.password.clone(), stored.phc).await {
        return Err(unauthorized());
    }
    // The password is proved before the account's state is mentioned, for the
    // reason `net::authenticate` documents.
    if stored.email_pending {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "confirm your email address first — check your inbox for the link".into(),
        ));
    }
    Ok(stored.principal)
}

/// Sign in to the admin API. Issues a token from the *same* session store the
/// game uses, so there is one notion of "signed in" per account.
async fn login(
    State(state): State<HttpState>,
    Json(body): Json<LoginRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let principal = check_password_login(&state, &body).await?;
    // Authenticated, but the API is for operators — an admin, or an account a
    // role gives something to run (`is_operator`): a player's correct password
    // gets the same answer as a wrong one, so this endpoint cannot be used to
    // tell which usernames exist.
    if !is_operator(caller_permissions(&state, principal)?) {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "wrong username or password".into(),
        ));
    }
    let token = state.sessions.issue(principal, &body.username);
    Ok(Json(json!({
        "token": token,
        "principal": principal,
        "username": body.username,
    })))
}

/// Sign in as a player, for the account pages that are not the game.
///
/// Until this route the only ways to hold a session in a browser were the
/// verification link (spent once, at sign-up) and the admin login (which refuses
/// players) — so an already-registered person could never reach the
/// claim-your-house screen again. The token is the ordinary session token, same
/// store and lifetime as the game's; `/api/buildings/*` takes it via
/// `require_session`, and the admin routes still refuse it.
async fn player_login(
    State(state): State<HttpState>,
    Json(body): Json<LoginRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let principal = check_password_login(&state, &body).await?;
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

/// The caller, if a person signed in with a password for this request: access
/// tokens are managed by the account, never by one another, so a stolen token
/// can neither mint its siblings nor revoke the one its owner would use to lock
/// it out (`docs/notes/access-tokens.md`).
pub(crate) fn require_person(state: &HttpState, headers: &HeaderMap) -> ApiResult<auth::Access> {
    let access = require_access(state, headers)?;
    if !access.is_session() {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "access tokens are managed from a sign-in, not with another token".into(),
        ));
    }
    Ok(access)
}

/// Ask a signed-in caller to prove it is them once more, before something a
/// session alone must not be enough for: their password, or — for an account
/// that has none — a sign-in through Discord, Google or the rest in the last
/// few minutes (`auth::REAUTH_WINDOW`).
pub(crate) async fn prove_again(
    state: &HttpState,
    access: &auth::Access,
    password: Option<String>,
) -> ApiResult<()> {
    use rusqlite::OptionalExtension as _;
    let wrong = || ApiError(StatusCode::FORBIDDEN, "that password is not right".into());
    let principal = access.principal;
    let phc: Option<String> = with_db(state, move |c| {
        Ok(c.query_row(
            "SELECT pw_hash FROM accounts WHERE id = ?1",
            [principal.to_string()],
            |r| r.get(0),
        )
        .optional()?)
    })?;
    let Some(phc) = phc else {
        return Err(wrong());
    };
    if !auth::has_password(&phc) {
        return match state.sessions.reauthed_recently(principal) {
            Some(_) => Ok(()),
            None => Err(ApiError(StatusCode::FORBIDDEN, REAUTH_NEEDED.into())),
        };
    }
    let Some(password) = password else {
        return Err(wrong());
    };
    let proved = check_password_login(
        state,
        &LoginRequest {
            username: access.username.clone(),
            password,
        },
    )
    .await
    .map_err(|_| wrong())?;
    if proved != principal {
        return Err(wrong());
    }
    Ok(())
}

/// The caller's access tokens: everything but the secrets.
async fn list_tokens(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let access = require_person(&state, &headers)?;
    Ok(Json(
        json!({ "tokens": state.sessions.access_tokens(access.principal) }),
    ))
}

#[derive(Deserialize)]
struct CreateTokenRequest {
    label: String,
    /// Asked again although the caller is signed in: a session can be a
    /// remembered one on a device somebody else is holding, and a token made
    /// there outlives the device — the reasoning `DeleteAccount` follows.
    /// Absent for an account without one, which signs in again through
    /// Discord, Google or the rest instead ([`prove_again`]).
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    elevated: bool,
    /// Unix seconds; absent for a token that lasts until it is revoked.
    #[serde(default)]
    expires_at: Option<u64>,
}

/// Make an access token. The answer carries the raw token, and nothing will
/// ever show it again.
async fn create_token(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<CreateTokenRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let access = require_person(&state, &headers)?;
    prove_again(&state, &access, body.password).await?;
    let (token, info) = state
        .sessions
        .create_access_token(
            access.principal,
            &access.username,
            &body.label,
            body.elevated,
            body.expires_at,
        )
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    tracing::info!(
        account = %access.username,
        id = %info.id,
        elevated = info.elevated,
        "access token created"
    );
    Ok(Json(json!({ "token": token, "info": info })))
}

/// Revoke one of the caller's access tokens. It stops working at once, on the
/// wire's next handshake and on HTTP's next request; a connection it already
/// opened carries on until it drops, as a disabled account's does.
async fn revoke_token(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<StatusCode> {
    let access = require_person(&state, &headers)?;
    if !state.sessions.revoke_access_token(access.principal, &id) {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such token".into()));
    }
    tracing::info!(account = %access.username, %id, "access token revoked");
    Ok(StatusCode::NO_CONTENT)
}

async fn me(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let access = require_access(&state, &headers)?;
    let (principal, username) = (access.principal, access.username.clone());
    let perms = access_permissions(&state, &access)?;
    if !is_operator(perms) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "that account may not administer this server".into(),
        ));
    }
    // `permissions` is what the web UI shows tabs by and what atlas checks
    // `pipeline_run` and `building_override` against.
    Ok(Json(json!({
        "principal": principal,
        "username": username,
        "admin": is_access_admin(&state, &access)?,
        "permissions": perms.slugs(),
    })))
}

async fn list_accounts(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    require(&state, &headers, Permission::AccountsManage)?;
    let accounts = with_db(&state, auth::list_accounts)?;
    // Beside the rows rather than in them: `AccountRow` is pinned field for field
    // to the web UI's `Account` type, and a role is not a column of the account.
    let roles = with_db(&state, crate::roles::all_account_roles)?;
    Ok(Json(json!({ "accounts": accounts, "roles": roles })))
}

/// Refuse a change that only an admin may make: anything to an admin's account,
/// and granting or revoking the flag itself. A role that may manage accounts is
/// otherwise a way to make yourself an admin, or to lock the admins out.
fn admin_only(state: &HttpState, caller: Uuid, why: bool) -> ApiResult<()> {
    if why && !is_caller_admin(state, caller)? {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "only an admin may change an admin's account or who is an admin".into(),
        ));
    }
    Ok(())
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
    let (caller, _) = require(&state, &headers, Permission::AccountsManage)?;
    admin_only(&state, caller, body.admin)?;
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
    /// An AI agent speaks for the account: everybody sees its badge.
    #[serde(default)]
    agent: Option<bool>,
}

async fn update_account(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<UpdateRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let (caller, _) = require(&state, &headers, Permission::AccountsManage)?;
    let target = with_db(&state, |c| auth::get_account(c, &id))?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "no such account".into()))?;
    admin_only(&state, caller, target.admin || body.admin.is_some())?;

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
    // Shown from the next contact list or call roster the account appears in.
    if let Some(agent) = body.agent {
        with_db(&state, |c| auth::set_agent(c, &id, agent))?;
    }

    if body.admin.is_some() || body.disabled.is_some() {
        crate::roles::refresh_grants(&state);
    }
    let account = with_db(&state, |c| auth::get_account(c, &id))?;
    tracing::info!(%caller, target = %id, "account updated");
    Ok(Json(json!({ "account": account })))
}

/// Confirm a registration whose verification mail never arrived, and tell the
/// person it is done.
///
/// `mailed` in the answer is whether the confirmation actually left this
/// process: `false` with no relay configured or when the relay refused it, so
/// the operator knows they have to tell the person some other way. The
/// activation itself stands either way.
async fn activate_account(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let (caller, _) = require(&state, &headers, Permission::AccountsManage)?;
    let outcome = with_db_mut(&state, |c| registration::activate(c, &id))?;
    let (username, email) = match outcome {
        registration::Activate::Activated { username, email } => (username, email),
        registration::Activate::NotPending => {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "that account is not waiting for confirmation".into(),
            ))
        }
        registration::Activate::Unknown => {
            return Err(ApiError(StatusCode::NOT_FOUND, "no such account".into()))
        }
    };
    let (subject, text) = mail::activated_message(&username, &state.config.public_base_url);
    let mailed = match state.mailer.send(&email, &subject, &text).await {
        Ok(()) => state.mailer.delivers(),
        Err(e) => {
            tracing::warn!("could not send {subject:?}: {e:#}");
            false
        }
    };
    tracing::info!(%caller, %username, mailed, "account activated by an operator");
    let account = with_db(&state, |c| auth::get_account(c, &id))?;
    Ok(Json(json!({ "account": account, "mailed": mailed })))
}

async fn remove_account(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<StatusCode> {
    let (caller, _) = require(&state, &headers, Permission::AccountsManage)?;
    if caller.to_string() == id {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "you cannot delete the account you are signed in with".into(),
        ));
    }
    let target = with_db(&state, |c| auth::get_account(c, &id))?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "no such account".into()))?;
    admin_only(&state, caller, target.admin)?;
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
// Owning buildings
// ---------------------------------------------------------------------------

/// The caller's principal, if they presented a live token. **No admin check** —
/// this is a player acting on their own account.
///
/// The distinction from [`require`] is the whole security model of these
/// three routes: everything they do is scoped to the principal this returns, so
/// there is no id in any request body that could name somebody else's claim.
pub(crate) fn require_session(state: &HttpState, headers: &HeaderMap) -> ApiResult<(Uuid, String)> {
    require_access(state, headers).map(|a| (a.principal, a.username))
}

/// [`require_session`], keeping what kind of credential it was — for a route
/// that decides on a permission (`access_permissions`) or that only a person
/// may use (`auth::Access::is_session`).
pub(crate) fn require_access(state: &HttpState, headers: &HeaderMap) -> ApiResult<auth::Access> {
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
    let access = state.sessions.resolve_access(token).ok_or(ApiError(
        StatusCode::UNAUTHORIZED,
        "that session has expired".to_string(),
    ))?;
    // A call's guest has a session for the call alone (`net::calls`), never
    // for the rest of the server: no pictures, no buildings, no profile.
    let now = auth::unix_now_s();
    let standing = with_db(state, move |c| auth::standing(c, access.principal, now))?;
    if standing != auth::Standing::Member {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "a guest in a call can do only that".to_string(),
        ));
    }
    Ok(access)
}

fn claims_open(state: &HttpState) -> ApiResult<()> {
    if state.config.house_claims_open {
        return Ok(());
    }
    Err(ApiError(
        StatusCode::FORBIDDEN,
        "this server does not hand out buildings".into(),
    ))
}

#[derive(serde::Deserialize)]
struct BuildingQuery {
    q: String,
}

/// Addresses matching what the person typed, each marked available or not.
///
/// Reaches two services somebody else runs, so it is throttled per caller on the
/// same limiter the registration paths use: a search box that fires per keystroke
/// against a public register is how a keyless service stops being keyless.
async fn search_buildings(
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<BuildingQuery>,
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
    let found = tokio::task::spawn_blocking(move || buildings::search(&q, &municipality))
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
    let taken = with_db(&state, |c| buildings::taken(c, &ids))?;
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

/// Claim the building at one address for the signed-in account.
///
/// The id from the search is treated as a *hint*, not as a fact: the address is
/// re-resolved from the register here, the municipality re-checked against the
/// server's own setting, and the building looked up from the address. Otherwise
/// the id is just a field, and a claim in another province is one `curl` away
/// from a rule that only the search box enforces.
async fn claim_building(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<ClaimRequest>,
) -> ApiResult<Response> {
    let (principal, username) = require(&state, &headers, Permission::BuildingClaim)?;
    claims_open(&state)?;

    let municipality = state.config.claim_municipality.clone();
    let id = body.nummeraanduiding_id.trim().to_string();
    let resolved = {
        let id = id.clone();
        let municipality = municipality.clone();
        tokio::task::spawn_blocking(move || buildings::lookup(&id, &municipality))
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
    let pand = tokio::task::spawn_blocking(move || buildings::pand_for(&object_id))
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

    let limit = state.config.max_buildings_per_account;
    let outcome = with_db(&state, |c| {
        buildings::claim(c, principal, &address, &pand, limit)
    })?;
    match outcome {
        buildings::Claimed::Ok => {
            tracing::info!(%username, address = %address.address, pand = %pand, "building claimed");
            let building = with_db(&state, |c| buildings::ownership(c, &pand))?.map(|mut o| {
                o.username = None;
                o
            });
            Ok((StatusCode::CREATED, Json(json!({ "building": building }))).into_response())
        }
        buildings::Claimed::Taken => Err(ApiError(
            StatusCode::CONFLICT,
            "somebody already owns that building".into(),
        )),
        buildings::Claimed::AlreadyYours => Err(ApiError(
            StatusCode::CONFLICT,
            "you already own that building".into(),
        )),
        buildings::Claimed::AtLimit { limit } => Err(ApiError(
            StatusCode::CONFLICT,
            format!("you already own {limit} buildings, which is as many as one account may"),
        )),
    }
}

async fn my_buildings(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let (principal, _) = require_session(&state, &headers)?;
    let owned = with_db(&state, |c| buildings::owned_by(c, principal))?;
    Ok(Json(json!({
        "buildings": owned,
        "limit": state.config.max_buildings_per_account,
    })))
}

async fn release_building(
    State(state): State<HttpState>,
    Path(pand): Path<String>,
    headers: HeaderMap,
) -> ApiResult<StatusCode> {
    let (principal, username) = require_session(&state, &headers)?;
    if !with_db(&state, |c| buildings::release(c, principal, &pand))? {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "you do not own that building".into(),
        ));
    }
    tracing::info!(%username, %pand, "building released");
    Ok(StatusCode::NO_CONTENT)
}

/// "May the bearer of this token change this building?" — the question atlas
/// asks before it saves a building design, answered here because ownership is
/// this server's (atlas stores the design and nothing about who may edit it).
///
/// Any live session gets an answer, and the answer is about the caller only, so
/// it leaks nothing a person could not learn by trying to save: whether *they*
/// own it or are an admin. The owner's name is included because the editor shows
/// whose building a visitor is looking at, which the game's building card will
/// too.
async fn building_access(
    State(state): State<HttpState>,
    Path(pand): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let access = require_access(&state, &headers)?;
    let (principal, username) = (access.principal, access.username.clone());
    let admin = is_access_admin(&state, &access)?;
    let perms = access_permissions(&state, &access)?;
    let row = with_db(&state, |c| buildings::ownership(c, &pand))?;
    let owner = with_db(&state, |c| buildings::owner_of(c, &pand))?
        .is_some_and(|p| p == principal.to_string());
    // Atlas believes this answer for designs and for exterior overrides, so the
    // roles decide here: an owner draws their own building's inside with
    // `building_design_own`, anybody's with `building_design_any`; replacing the
    // outside is `building_override` alone, whoever owns the building.
    let can_edit = (owner && perms.has(Permission::BuildingDesignOwn))
        || perms.has(Permission::BuildingDesignAny);
    Ok(Json(json!({
        "username": username,
        "admin": admin,
        "owner": owner,
        "can_edit": can_edit,
        "can_override": perms.has(Permission::BuildingOverride),
        "owned_by": row.and_then(|r| r.username),
    })))
}

/// Every owned building, for the operator. The dispute-resolution surface:
/// claiming is first come first served, so the only answer to "that is my
/// house" is a person with this list and the button below it.
async fn list_claims(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    require(&state, &headers, Permission::ReportsTriage)?;
    let owned = with_db(&state, buildings::list)?;
    Ok(Json(json!({ "buildings": owned })))
}

async fn release_claim(
    State(state): State<HttpState>,
    Path(pand): Path<String>,
    headers: HeaderMap,
) -> ApiResult<StatusCode> {
    let (caller, _) = require(&state, &headers, Permission::ReportsTriage)?;
    if !with_db(&state, |c| buildings::release_building(c, &pand))? {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "nobody owns that building".into(),
        ));
    }
    tracing::info!(%caller, %pand, "building released by an operator");
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
    require(&state, &headers, Permission::ReportsTriage)?;
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
    let (caller, _) = require(&state, &headers, Permission::ReportsTriage)?;
    let ok = with_db(&state, |c| {
        crate::db::resolve_report(c, id, &body.resolution)
    })?;
    if !ok {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such report".into()));
    }
    tracing::info!(%caller, report_id = id, "report resolved");
    Ok(StatusCode::NO_CONTENT)
}
