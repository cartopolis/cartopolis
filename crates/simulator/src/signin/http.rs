//! The HTTP side of signing in with another account: the round trip, its
//! ticket, the app's pairing, the account's own list of sign-ins, Admin ›
//! Sign-in, and communities' linked groups. The shapes are what
//! `crates/atlas/web/src/types.ts` mirrors.

use std::net::SocketAddr;
use std::time::Instant;

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use cartopolis_core::permissions::Permission;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::providers::{self, Credentials, Endpoints};
use super::store::{self, Provider};
use super::{Flow, Kind, NewAccounts, Outcome, Profile, Purpose, Ticket};
use crate::auth;
use crate::http::{
    bad_request, caller_key, check_password, check_password_login, check_username, internal,
    prove_again, require, require_person, require_session, with_db, with_db_mut, ApiError,
    ApiResult, HttpState, LoginRequest,
};

pub fn routes() -> Router<HttpState> {
    Router::new()
        .route("/api/auth/app", post(app_pair))
        .route("/api/auth/app/{code}", get(app_poll))
        .route("/api/auth/ticket", post(read_ticket))
        .route("/api/auth/ticket/create", post(ticket_create))
        .route("/api/auth/ticket/link", post(ticket_link))
        .route("/api/auth/ticket/join", post(ticket_join))
        .route("/api/auth/ticket/community", post(ticket_community))
        .route("/api/auth/{id}/start", get(start))
        .route("/api/auth/{id}/begin", post(begin))
        .route("/api/auth/{id}/callback", get(callback))
        .route("/api/account/signin", get(account_signin))
        .route("/api/account/identities/{id}", delete(account_unlink))
        .route("/api/account/password", post(account_password))
        .route("/api/admin/signin", get(admin_list))
        .route("/api/admin/signin/order", post(admin_order))
        .route("/api/admin/signin/{id}", put(admin_save).delete(admin_delete))
        .route("/api/communities/{id}/links", get(community_links))
        .route(
            "/api/communities/{id}/links/{provider}/{group}",
            delete(community_unlink),
        )
}

fn now_s() -> i64 {
    auth::unix_now_s() as i64
}

fn base(state: &HttpState) -> &str {
    state.config.public_base_url.trim_end_matches('/')
}

fn redirect_uri(state: &HttpState, id: &str) -> String {
    format!("{}/api/auth/{id}/callback", base(state))
}

/// A path on this site to come back to; anything else (another site, a
/// protocol-relative `//evil.example`) becomes the front page.
fn safe_return(r: Option<&str>) -> String {
    match r {
        Some(p) if p.starts_with('/') && !p.starts_with("//") && !p.contains('\\') => p.into(),
        _ => "/".into(),
    }
}

/// The web UI's `/welcome` with an error sentence, for a round trip that could
/// not start or did not come back.
fn welcome_error(state: &HttpState, message: &str) -> Response {
    let mut url = url::Url::parse(&format!("{}/welcome", base(state)))
        .unwrap_or_else(|_| url::Url::parse("http://localhost/welcome").expect("static URL"));
    url.query_pairs_mut().append_pair("error", message);
    Redirect::to(url.as_str()).into_response()
}

fn conflict(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::CONFLICT, msg.into())
}

fn refusal(r: crate::communities::Refusal) -> ApiError {
    use crate::communities::Refusal;
    let status = match r {
        Refusal::NoSuchCommunity => StatusCode::NOT_FOUND,
        Refusal::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::FORBIDDEN,
    };
    ApiError(status, r.to_string())
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| anyhow::anyhow!("sign-in task: {e}"))?
}

fn live_provider(state: &HttpState, id: &str) -> ApiResult<Provider> {
    let id = id.to_string();
    with_db(state, move |c| store::provider(c, &id))?
        .filter(Provider::live)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "this server does not offer that".into()))
}

fn provider_name(state: &HttpState, id: &str) -> String {
    let key = id.to_string();
    with_db(state, move |c| store::provider(c, &key))
        .ok()
        .flatten()
        .map_or_else(|| id.to_string(), |p| p.name)
}

// ── Starting a round trip ──────────────────────────────────────────────────

/// What the browser needs to be sent to the provider: its address, and the
/// keys the code will be exchanged with afterwards.
struct Prepared {
    endpoints: Endpoints,
    client_id: String,
    host: Option<String>,
}

/// The fediverse server's origin: `https`, except a test's stand-in on this
/// machine (`host:port` with private addresses allowed), as `webfinger` does.
fn fediverse_origin(state: &HttpState, host: &str) -> String {
    if state.signin.client.allows_private() && host.contains(':') {
        format!("http://{host}")
    } else {
        format!("https://{host}")
    }
}

/// Find, and for a new fediverse server make, what `provider` needs.
async fn prepare(state: &HttpState, p: &Provider, server: Option<&str>) -> Result<Prepared, String> {
    match p.kind {
        Kind::Oidc => {
            let issuer = p.issuer.clone().unwrap_or_default();
            let signin = state.signin.clone();
            let endpoints = blocking(move || signin.endpoints(&issuer))
                .await
                .map_err(|e| {
                    tracing::warn!(provider = %p.id, "discovery: {e:#}");
                    format!("{} did not answer. Try again in a moment.", p.name)
                })?;
            Ok(Prepared {
                endpoints,
                client_id: p.client_id.clone(),
                host: None,
            })
        }
        Kind::Mastodon => {
            let host = server
                .and_then(providers::fediverse_host)
                .ok_or("That doesn't look like a fediverse address or server.")?;
            let blocked = {
                let h = host.clone();
                with_db(state, move |c| Ok(crate::fediverse::blocklist::is_blocked(c, &h)?))
                    .unwrap_or(true)
            };
            if blocked {
                return Err(format!("This server doesn't take sign-ins from {host}."));
            }
            let origin = fediverse_origin(state, &host);
            let known = {
                let h = host.clone();
                with_db(state, move |c| store::fediverse_app(c, &h)).map_err(|e| e.1)?
            };
            let client_id = match known {
                Some((id, _)) => id,
                None => {
                    let (client, redirect, site) = (
                        state.signin.client.clone(),
                        redirect_uri(state, &p.id),
                        base(state).to_string(),
                    );
                    let o = origin.clone();
                    let (id, secret) = blocking(move || {
                        providers::register_mastodon_app(&client, &o, &redirect, &site)
                    })
                    .await
                    .map_err(|e| {
                        tracing::info!(%host, "fediverse sign-in registration: {e:#}");
                        format!("{host} didn't let this server sign you in. Is it a Mastodon-compatible server?")
                    })?;
                    let sealed = state
                        .signin
                        .key
                        .seal(&secret, &format!("fediverse:{host}"))
                        .map_err(|e| e.to_string())?;
                    let (h, i) = (host.clone(), id.clone());
                    with_db(state, move |c| {
                        store::save_fediverse_app(c, &h, &i, &sealed, now_s())
                    })
                    .map_err(|e| e.1)?;
                    id
                }
            };
            Ok(Prepared {
                endpoints: providers::mastodon_endpoints(&origin),
                client_id,
                host: Some(host),
            })
        }
        kind => Ok(Prepared {
            endpoints: providers::fixed_endpoints(kind, state.signin.test_base.as_deref())
                .ok_or("unknown provider")?,
            client_id: p.client_id.clone(),
            host: None,
        }),
    }
}

/// Remember the round trip and answer the provider's address to send the
/// browser to.
async fn launch(
    state: &HttpState,
    p: &Provider,
    purpose: Purpose,
    server: Option<&str>,
    app: Option<String>,
    return_to: String,
) -> Result<String, String> {
    let prepared = prepare(state, p, server).await?;
    let managed = matches!(purpose, Purpose::Community { .. });
    let groups = !managed && purpose == Purpose::SignIn && p.kind.has_groups() && {
        let id = p.id.clone();
        with_db(state, move |c| store::any_links(c, &id)).unwrap_or(false)
    };
    let verifier = auth::random_token();
    let flow_state = state.signin.remember(Flow {
        provider: p.id.clone(),
        purpose,
        verifier: verifier.clone(),
        host: prepared.host,
        app,
        return_to,
        want_groups: groups || managed,
        created: Instant::now(),
    });
    Ok(providers::authorize_url(
        p.kind,
        &prepared.endpoints,
        &prepared.client_id,
        &redirect_uri(state, &p.id),
        providers::scopes(p.kind, groups, managed),
        &flow_state,
        &verifier,
    ))
}

#[derive(Deserialize)]
struct StartQuery {
    #[serde(rename = "return")]
    return_to: Option<String>,
    app: Option<String>,
    server: Option<String>,
}

/// Somebody without a session presses a provider's button: off to the
/// provider. A browser navigation, so every answer is a redirect.
async fn start(
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<StartQuery>,
) -> Response {
    if !state.signin.starts.allow(&caller_key(&headers, peer)) {
        return welcome_error(&state, "Too many sign-ins from here. Wait a while and try again.");
    }
    let Ok(p) = live_provider(&state, &id) else {
        return welcome_error(&state, "This server doesn't offer that way to sign in.");
    };
    let app = q.app.filter(|c| !c.is_empty());
    if let Some(code) = &app {
        if !state.signin.pairing_waiting(code) {
            return welcome_error(
                &state,
                "That sign-in from the app has expired. Start again from the app.",
            );
        }
    }
    let return_to = safe_return(q.return_to.as_deref());
    match launch(&state, &p, Purpose::SignIn, q.server.as_deref(), app, return_to).await {
        Ok(url) => Redirect::to(&url).into_response(),
        Err(message) => welcome_error(&state, &message),
    }
}

#[derive(Deserialize)]
struct BeginBody {
    purpose: String,
    #[serde(default)]
    community: Option<Uuid>,
    #[serde(default)]
    server: Option<String>,
    #[serde(default, rename = "return")]
    return_to: Option<String>,
}

/// A signed-in caller starts a round trip: to add a provider, to prove it is
/// them, to test a provider's keys, or to link a community to a group. Answers
/// the address; the web UI navigates there.
async fn begin(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<BeginBody>,
) -> ApiResult<Json<Value>> {
    let return_to = safe_return(body.return_to.as_deref());
    let (p, purpose) = match body.purpose.as_str() {
        "test" => {
            let (principal, _) = require(&state, &headers, Permission::SigninManage)?;
            let pid = id.clone();
            let p = with_db(&state, move |c| store::provider(c, &pid))?
                .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "no such provider".into()))?;
            if !p.has_keys() {
                return Err(conflict("Enter its client id and secret first."));
            }
            (p, Purpose::Test(principal))
        }
        "link" | "reauth" => {
            let access = require_person(&state, &headers)?;
            let p = live_provider(&state, &id)?;
            if body.purpose == "link" {
                (p, Purpose::Link(access.principal))
            } else {
                (p, Purpose::Reauth(access.principal))
            }
        }
        "community" => {
            let (principal, _) = require_session(&state, &headers)?;
            let p = live_provider(&state, &id)?;
            if !p.kind.has_groups() {
                return Err(bad_request("only a Discord server or a subreddit can be linked"));
            }
            let community = body
                .community
                .ok_or_else(|| bad_request("which community?"))?;
            let by = principal.to_string();
            with_db(&state, move |c| {
                Ok(crate::communities::may_moderate(c, community, &by))
            })?
            .map_err(refusal)?;
            (
                p,
                Purpose::Community {
                    by: principal,
                    community,
                },
            )
        }
        _ => return Err(bad_request("unknown purpose")),
    };
    let url = launch(&state, &p, purpose, body.server.as_deref(), None, return_to)
        .await
        .map_err(|m| ApiError(StatusCode::BAD_GATEWAY, m))?;
    Ok(Json(json!({ "url": url })))
}

// ── Coming back ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// The provider sends the browser back here. Whatever happened, the browser
/// goes on to `/welcome`, with a ticket that says what or with an error.
async fn callback(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    Query(q): Query<CallbackQuery>,
) -> Response {
    let Some(flow) = q
        .state
        .as_deref()
        .and_then(|s| state.signin.take_flow(s))
        .filter(|f| f.provider == id)
    else {
        return welcome_error(
            &state,
            "That sign-in took too long or was used already. Please try again.",
        );
    };
    if q.error.is_some() || q.code.is_none() {
        return welcome_error(&state, "Signing in was cancelled.");
    }
    let code = q.code.unwrap_or_default();
    match finish(&state, &id, &flow, &code).await {
        Ok(ticket) => {
            let id = state.signin.file(ticket);
            Redirect::to(&format!("{}/welcome?ticket={id}", base(&state))).into_response()
        }
        Err(message) => welcome_error(&state, &message),
    }
}

/// Exchange the code, read the profile, and decide what it means.
async fn finish(state: &HttpState, id: &str, flow: &Flow, code: &str) -> Result<Ticket, String> {
    let p = {
        let id = id.to_string();
        with_db(state, move |c| store::provider(c, &id))
            .ok()
            .flatten()
            .ok_or("This server no longer offers that way to sign in.")?
    };
    let testing = matches!(flow.purpose, Purpose::Test(_));
    if !testing && !p.live() {
        return Err("This server no longer offers that way to sign in.".into());
    }
    // The keys: the operator's, or the ones the fediverse server issued.
    let (client_id, secret, endpoints) = if p.kind == Kind::Mastodon {
        let host = flow.host.clone().unwrap_or_default();
        let h = host.clone();
        let (cid, sealed) = with_db(state, move |c| store::fediverse_app(c, &h))
            .ok()
            .flatten()
            .ok_or("This server lost track of that sign-in. Please try again.")?;
        let secret = state
            .signin
            .key
            .open(&sealed, &format!("fediverse:{host}"))
            .map_err(|e| e.to_string())?;
        (cid, secret, providers::mastodon_endpoints(&fediverse_origin(state, &host)))
    } else {
        let secret = p
            .secret_sealed
            .as_deref()
            .map(|s| state.signin.key.open(s, &p.id))
            .transpose()
            .map_err(|e| {
                tracing::warn!(provider = %p.id, "{e:#}");
                "This server's keys for that provider can't be read. Its operator has to enter them again."
                    .to_string()
            })?
            .unwrap_or_default();
        let prepared = prepare(state, &p, None).await?;
        (p.client_id.clone(), secret, prepared.endpoints)
    };
    let (client, kind, redirect, verifier, host) = (
        state.signin.client.clone(),
        p.kind,
        redirect_uri(state, &p.id),
        flow.verifier.clone(),
        flow.host.clone(),
    );
    let managed = matches!(flow.purpose, Purpose::Community { .. });
    let groups = flow.want_groups && !managed;
    let code = code.to_string();
    let profile = blocking(move || {
        let token = providers::exchange(
            &client,
            kind,
            &endpoints,
            &Credentials {
                client_id: &client_id,
                client_secret: &secret,
            },
            &code,
            &redirect,
            &verifier,
        )?;
        providers::profile(&client, kind, &endpoints, &token, host.as_deref(), groups, managed)
    })
    .await
    .map_err(|e| {
        tracing::info!(provider = %p.id, "sign-in failed: {e:#}");
        if testing {
            format!("The test did not pass: {e}")
        } else {
            format!("Signing in with {} didn't work. Please try again.", p.name)
        }
    })?;
    let outcome = decide(state, &p, flow, &profile).await?;
    Ok(Ticket {
        provider: p.id.clone(),
        provider_name: p.name.clone(),
        kind: p.kind,
        profile,
        outcome,
        purpose: flow.purpose.clone(),
        app: flow.app.clone(),
        return_to: flow.return_to.clone(),
        created: Instant::now(),
    })
}

fn group_ids(profile: &Profile) -> Vec<String> {
    profile.groups.iter().map(|g| g.group_id.clone()).collect()
}

/// A session for the app that started this round trip, left for it to
/// collect: its own, not the browser's, so signing out of one leaves the other.
fn hand_to_app(state: &HttpState, app: Option<&str>, principal: Uuid, username: &str) {
    if let Some(code) = app {
        let token = state.sessions.issue(principal, username);
        state
            .signin
            .complete_pairing(code, token, username.to_string());
    }
}

async fn decide(
    state: &HttpState,
    p: &Provider,
    flow: &Flow,
    profile: &Profile,
) -> Result<Outcome, String> {
    let db_err = |e: ApiError| e.1;
    let (pid, subject) = (p.id.clone(), profile.subject.clone());
    let linked = with_db(state, move |c| store::find_identity(c, &pid, &subject)).map_err(db_err)?;
    Ok(match &flow.purpose {
        Purpose::Test(_) => {
            let id = p.id.clone();
            with_db(state, move |c| {
                if let Some(p) = store::provider(c, &id)? {
                    store::save_provider(
                        c,
                        &Provider {
                            tested_at: Some(now_s()),
                            ..p
                        },
                    )?;
                }
                Ok(())
            })
            .map_err(db_err)?;
            Outcome::Tested
        }
        Purpose::Reauth(principal) => match linked {
            Some((who, _)) if who == *principal => {
                state.sessions.mark_reauthed(*principal);
                Outcome::Reauthed
            }
            _ => Outcome::Refused(format!(
                "That isn't the {} account linked to yours here.",
                p.name
            )),
        },
        Purpose::Link(principal) => {
            let (pid, subject, handle, who) = (
                p.id.clone(),
                profile.subject.clone(),
                profile.handle.clone(),
                *principal,
            );
            let added = with_db(state, move |c| {
                store::add_identity(c, &pid, &subject, who, &handle, now_s())
            })
            .map_err(db_err)?;
            if added {
                Outcome::Linked
            } else {
                Outcome::Refused(format!(
                    "That {} account is linked to another account here already.",
                    p.name
                ))
            }
        }
        Purpose::Community { community, .. } => {
            let c_id = *community;
            let name: Option<String> = with_db(state, move |c| {
                use rusqlite::OptionalExtension as _;
                Ok(c.query_row(
                    "SELECT name FROM communities WHERE id = ?1",
                    [c_id.to_string()],
                    |r| r.get(0),
                )
                .optional()?)
            })
            .map_err(db_err)?;
            Outcome::CommunityGroups {
                community: *community,
                community_name: name.ok_or("That community is gone.")?,
            }
        }
        Purpose::SignIn => {
            if let Some((principal, username)) = linked {
                let token = state.sessions.issue(principal, &username);
                hand_to_app(state, flow.app.as_deref(), principal, &username);
                let (pid, groups) = (p.id.clone(), group_ids(profile));
                let offers = with_db(state, move |c| store::offers(c, &pid, &groups, Some(principal)))
                    .map_err(db_err)?;
                tracing::info!(provider = %p.id, %username, "signed in");
                Outcome::SignedIn {
                    principal,
                    username,
                    token: Some(token),
                    offers,
                }
            } else {
                new_person(state, p, profile).await?
            }
        }
    })
}

/// Somebody nobody here knows signed in: an account to make, an account to
/// link, or a refusal.
async fn new_person(state: &HttpState, p: &Provider, profile: &Profile) -> Result<Outcome, String> {
    let db_err = |e: ApiError| e.1;
    if let Some(email) = profile.email.clone() {
        let owner = with_db(state, move |c| {
            let Some(username) = store::email_owner(c, &email)? else {
                return Ok(None);
            };
            let principal = auth::lookup_credential(c, &username)?.map(|s| s.principal);
            let has = match principal {
                Some(id) => store::has_password(c, id)?,
                None => false,
            };
            Ok(Some((username, has)))
        })
        .map_err(db_err)?;
        match owner {
            Some((username, true)) => return Ok(Outcome::LinkExisting { username }),
            Some((username, false)) => {
                return Ok(Outcome::Refused(format!(
                    "That address belongs to the account {username}, which signs in another way. \
                     Sign in that way, then add {} from your account page.",
                    p.name
                )))
            }
            None => {}
        }
    }
    if !state.config.registration_open {
        return Ok(Outcome::Refused(
            "This server doesn't make new accounts. Ask its operator for one.".into(),
        ));
    }
    let (pid, groups) = (p.id.clone(), group_ids(profile));
    let offers = with_db(state, move |c| store::offers(c, &pid, &groups, None)).map_err(db_err)?;
    match p.new_accounts {
        NewAccounts::Nobody => Ok(Outcome::Refused(format!(
            "New accounts can't be made with {} here. If you have an account, sign in with it \
             and add {} from your account page.",
            p.name, p.name
        ))),
        NewAccounts::Members if offers.is_empty() => Ok(Outcome::Refused(format!(
            "New accounts with {} are only for members of the groups this server's communities \
             are linked to.",
            p.name
        ))),
        new_accounts => Ok(Outcome::ChooseName {
            suggestion: super::suggest_name(profile),
            offers,
            new_accounts,
        }),
    }
}

// ── Reading and spending a ticket ──────────────────────────────────────────

#[derive(Deserialize)]
struct TicketBody {
    ticket: String,
}

fn identity_json(t: &Ticket) -> Value {
    let letter = t
        .profile
        .display_name
        .chars()
        .chain(t.profile.handle.chars())
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    json!({
        "provider": t.provider,
        "provider_name": t.provider_name,
        "kind": t.kind.as_str(),
        "handle": t.profile.handle,
        "display_name": t.profile.display_name,
        "email": t.profile.email,
        "email_verified": t.profile.email.is_some(),
        "avatar_letter": letter,
    })
}

fn find_ticket(state: &HttpState, id: &str) -> ApiResult<Ticket> {
    state.signin.ticket(id).ok_or_else(|| {
        ApiError(
            StatusCode::GONE,
            "That sign-in took too long. Please start again.".into(),
        )
    })
}

/// What a round trip came to, for `/welcome` to act on.
async fn read_ticket(
    State(state): State<HttpState>,
    Json(body): Json<TicketBody>,
) -> ApiResult<Json<Value>> {
    let t = find_ticket(&state, &body.ticket)?;
    let app_code = t.app.clone();
    Ok(Json(match &t.outcome {
        Outcome::SignedIn {
            principal,
            username,
            token,
            offers,
        } => {
            // The browser's session, handed over once: a ticket is in the
            // address bar and the history, and must not sign in whoever
            // reads it later.
            if token.is_some() {
                state.signin.update_ticket(&body.ticket, |t| {
                    if let Outcome::SignedIn { token, .. } = &mut t.outcome {
                        *token = None;
                    }
                });
            }
            json!({
                "kind": "signed_in",
                "token": token,
                "username": username,
                "principal": principal,
                "offers": offers,
                "app_code": app_code,
                "return": t.return_to,
            })
        }
        Outcome::ChooseName {
            suggestion,
            offers,
            new_accounts,
        } => json!({
            "kind": "choose_name",
            "identity": identity_json(&t),
            "suggestion": suggestion,
            "offers": offers,
            "new_accounts": new_accounts.as_str(),
            "app_code": app_code,
            "return": t.return_to,
        }),
        Outcome::LinkExisting { username } => json!({
            "kind": "link_existing",
            "identity": identity_json(&t),
            "username": username,
            "app_code": app_code,
            "return": t.return_to,
        }),
        Outcome::Linked => json!({
            "kind": "linked", "provider_name": t.provider_name, "return": t.return_to,
        }),
        Outcome::Reauthed => json!({
            "kind": "reauthed", "provider_name": t.provider_name, "return": t.return_to,
        }),
        Outcome::Tested => json!({ "kind": "tested", "provider_name": t.provider_name }),
        Outcome::CommunityGroups {
            community,
            community_name,
        } => json!({
            "kind": "community_groups",
            "community": { "id": community, "name": community_name },
            "provider": t.provider,
            "provider_name": t.provider_name,
            "groups": t.profile.managed,
        }),
        Outcome::Refused(message) => json!({ "kind": "refused", "message": message }),
        Outcome::Spent => json!({
            "kind": "refused",
            "message": "That sign-in has been used already. Sign in again if you need to.",
        }),
    }))
}

fn spend(state: &HttpState, id: &str) {
    state.signin.update_ticket(id, |t| t.outcome = Outcome::Spent);
}

/// Join the communities picked from a ticket's offers. Only those offered:
/// the list is the proof of being in the group.
fn join_offered(
    state: &HttpState,
    principal: Uuid,
    offers: &[store::Offer],
    picked: &[String],
) -> ApiResult<Vec<String>> {
    let wanted: Vec<Uuid> = offers
        .iter()
        .filter(|o| picked.contains(&o.community))
        .filter_map(|o| Uuid::parse_str(&o.community).ok())
        .collect();
    let account = principal.to_string();
    with_db(state, move |c| {
        let mut joined = Vec::new();
        for id in wanted {
            match crate::communities::join_linked(c, &account, id, crate::fediverse::now_ms()) {
                Ok(_) => joined.push(id.to_string()),
                // A block or a vanished community: the others still go ahead.
                Err(e) => tracing::info!(community = %id, "linked join refused: {e}"),
            }
        }
        Ok(joined)
    })
}

#[derive(Deserialize)]
struct CreateBody {
    ticket: String,
    username: String,
    #[serde(default)]
    join: Vec<String>,
}

/// Make the account a `choose_name` ticket offered.
async fn ticket_create(
    State(state): State<HttpState>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    let t = find_ticket(&state, &body.ticket)?;
    let Outcome::ChooseName { offers, .. } = &t.outcome else {
        return Err(conflict("That sign-in has been used already."));
    };
    if !state.config.registration_open {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "this server does not take registrations".into(),
        ));
    }
    let username = body.username.trim().to_string();
    check_username(&username)?;
    let (name, email, provider, subject, handle) = (
        username.clone(),
        t.profile.email.clone(),
        t.provider.clone(),
        t.profile.subject.clone(),
        t.profile.handle.clone(),
    );
    let made = with_db_mut(&state, move |c| {
        store::create_account(c, &name, email.as_deref(), &provider, &subject, &handle, now_s())
    })?;
    let principal = match made {
        store::Made::Created(p) => p,
        store::Made::NameTaken => {
            return Err(conflict(format!("the name {username} is taken — pick another")))
        }
        store::Made::EmailTaken | store::Made::AlreadyLinked => {
            return Err(conflict("That sign-in has been used already."))
        }
    };
    spend(&state, &body.ticket);
    tracing::info!(provider = %t.provider, %username, "account made through another sign-in");
    let joined = join_offered(&state, principal, offers, &body.join)?;
    let token = state.sessions.issue(principal, &username);
    hand_to_app(&state, t.app.as_deref(), principal, &username);
    Ok(Json(json!({
        "token": token,
        "username": username,
        "principal": principal,
        "joined": joined,
        "app_code": t.app,
    })))
}

#[derive(Deserialize)]
struct LinkBody {
    ticket: String,
    password: String,
}

/// Prove the existing account with its password once, and link the provider
/// to it.
async fn ticket_link(
    State(state): State<HttpState>,
    Json(body): Json<LinkBody>,
) -> ApiResult<Json<Value>> {
    let t = find_ticket(&state, &body.ticket)?;
    let Outcome::LinkExisting { username } = &t.outcome else {
        return Err(conflict("That sign-in has been used already."));
    };
    let principal = check_password_login(
        &state,
        &LoginRequest {
            username: username.clone(),
            password: body.password,
        },
    )
    .await
    .map_err(|_| ApiError(StatusCode::FORBIDDEN, "that password is not right".into()))?;
    let (provider, subject, handle) = (
        t.provider.clone(),
        t.profile.subject.clone(),
        t.profile.handle.clone(),
    );
    let added = with_db(&state, move |c| {
        store::add_identity(c, &provider, &subject, principal, &handle, now_s())
    })?;
    if !added {
        return Err(conflict(format!(
            "That {} account is linked to another account here already.",
            t.provider_name
        )));
    }
    spend(&state, &body.ticket);
    tracing::info!(provider = %t.provider, %username, "linked by password");
    let token = state.sessions.issue(principal, username);
    hand_to_app(&state, t.app.as_deref(), principal, username);
    Ok(Json(json!({
        "token": token,
        "username": username,
        "principal": principal,
        "app_code": t.app,
    })))
}

#[derive(Deserialize)]
struct JoinBody {
    ticket: String,
    #[serde(default)]
    join: Vec<String>,
}

/// Join communities offered after an ordinary sign-in.
async fn ticket_join(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<JoinBody>,
) -> ApiResult<Json<Value>> {
    let access = require_person(&state, &headers)?;
    let t = find_ticket(&state, &body.ticket)?;
    let Outcome::SignedIn {
        principal, offers, ..
    } = &t.outcome
    else {
        return Err(conflict("Nothing to join from that sign-in."));
    };
    if *principal != access.principal {
        return Err(ApiError(StatusCode::FORBIDDEN, "not your sign-in".into()));
    }
    let joined = join_offered(&state, *principal, offers, &body.join)?;
    Ok(Json(json!({ "joined": joined })))
}

#[derive(Deserialize)]
struct CommunityBody {
    ticket: String,
    group_id: String,
}

/// Link the community a `community_groups` ticket was for to one of the
/// groups it found the caller runs.
async fn ticket_community(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<CommunityBody>,
) -> ApiResult<Json<Value>> {
    let (caller, username) = require_session(&state, &headers)?;
    let t = find_ticket(&state, &body.ticket)?;
    let (Purpose::Community { by, community }, Outcome::CommunityGroups { .. }) =
        (&t.purpose, &t.outcome)
    else {
        return Err(conflict("That sign-in has been used already."));
    };
    if *by != caller {
        return Err(ApiError(StatusCode::FORBIDDEN, "not your sign-in".into()));
    }
    let group = t
        .profile
        .managed
        .iter()
        .find(|g| g.group_id == body.group_id)
        .cloned()
        .ok_or_else(|| {
            ApiError(
                StatusCode::FORBIDDEN,
                "you can only link a group you run".into(),
            )
        })?;
    let (community, provider) = (*community, t.provider.clone());
    let g = group.clone();
    let added = with_db(&state, move |c| {
        if let Err(r) = crate::communities::may_moderate(c, community, &caller.to_string()) {
            return Ok(Err(r));
        }
        Ok(Ok(store::add_link(
            c,
            community,
            &provider,
            &g.group_id,
            &g.name,
            caller,
            now_s(),
        )?))
    })?
    .map_err(refusal)?;
    if !added {
        return Err(conflict(format!(
            "{} is linked to another community already.",
            group.name
        )));
    }
    spend(&state, &body.ticket);
    Ok(Json(json!({
        "link": {
            "provider": t.provider,
            "provider_name": t.provider_name,
            "group_id": group.group_id,
            "group_name": group.name,
            "linked_by": username,
            "linked_at": now_s(),
        }
    })))
}

// ── The app's pairing ──────────────────────────────────────────────────────

async fn app_pair(
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    if !state.signin.starts.allow(&caller_key(&headers, peer)) {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "too many sign-ins from here — wait a while and try again".into(),
        ));
    }
    let (code, secret) = state.signin.pair();
    Ok(Json(json!({
        "code": code,
        "secret": secret,
        "expires_in": super::FLOW_TTL.as_secs(),
    })))
}

#[derive(Deserialize)]
struct PollQuery {
    secret: String,
}

async fn app_poll(
    State(state): State<HttpState>,
    Path(code): Path<String>,
    Query(q): Query<PollQuery>,
) -> ApiResult<Json<Value>> {
    match state.signin.poll_pairing(&code, &q.secret) {
        None => Err(ApiError(
            StatusCode::NOT_FOUND,
            "that sign-in has expired — start again".into(),
        )),
        Some(None) => Ok(Json(json!({ "status": "waiting" }))),
        Some(Some((token, username))) => Ok(Json(json!({
            "status": "done", "token": token, "username": username,
        }))),
    }
}

// ── The account's own sign-ins ─────────────────────────────────────────────

async fn account_signin(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    let access = require_person(&state, &headers)?;
    let principal = access.principal;
    let (has_password, identities, all) = with_db(&state, move |c| {
        Ok((
            store::has_password(c, principal)?,
            store::identities_of(c, principal)?,
            store::all_providers(c)?,
        ))
    })?;
    let name_of = |id: &str| {
        all.iter()
            .find(|p| p.id == id)
            .map_or((id.to_string(), "oidc"), |p| (p.name.clone(), p.kind.as_str()))
    };
    let identities: Vec<Value> = identities
        .iter()
        .map(|i| {
            let (name, kind) = name_of(&i.provider);
            json!({
                "id": i.id, "provider": i.provider, "provider_name": name, "kind": kind,
                "handle": i.handle, "linked_at": i.linked_at,
            })
        })
        .collect();
    let available: Vec<Value> = all
        .iter()
        .filter(|p| p.live())
        .map(|p| json!({ "id": p.id, "kind": p.kind.as_str(), "name": p.name }))
        .collect();
    let reauth_until = state.sessions.reauthed_recently(principal).map(|at| {
        now_s() + (auth::REAUTH_WINDOW.saturating_sub(at.elapsed())).as_secs() as i64
    });
    Ok(Json(json!({
        "has_password": has_password,
        "identities": identities,
        "available": available,
        "reauth_until": reauth_until,
    })))
}

async fn account_unlink(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let access = require_person(&state, &headers)?;
    let principal = access.principal;
    match with_db(&state, move |c| store::remove_identity(c, principal, &id))? {
        store::Unlink::Removed => Ok(StatusCode::NO_CONTENT),
        store::Unlink::Unknown => Err(ApiError(StatusCode::NOT_FOUND, "no such sign-in".into())),
        store::Unlink::LastWayIn => Err(conflict(
            "This is your only way to sign in. Link another account or add a password first.",
        )),
    }
}

#[derive(Deserialize)]
struct PasswordBody {
    password: String,
    #[serde(default)]
    current_password: Option<String>,
}

/// Add a password to an account that started without one (after signing in
/// again through its provider), or change one (with the current one).
async fn account_password(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<PasswordBody>,
) -> ApiResult<StatusCode> {
    let access = require_person(&state, &headers)?;
    check_password(&body.password)?;
    prove_again(&state, &access, body.current_password).await?;
    let password = body.password;
    let hash = tokio::task::spawn_blocking(move || auth::hash_password(&password))
        .await
        .map_err(|e| internal(anyhow::anyhow!("hash task: {e}")))?
        .map_err(internal)?;
    let id = access.principal.to_string();
    with_db(&state, move |c| {
        c.execute(
            "UPDATE accounts SET pw_hash = ?2 WHERE id = ?1",
            rusqlite::params![id, hash],
        )?;
        Ok(())
    })?;
    tracing::info!(account = %access.username, "password set");
    Ok(StatusCode::NO_CONTENT)
}

// ── Admin › Sign-in ────────────────────────────────────────────────────────

fn admin_json(state: &HttpState, p: &Provider, usage: (i64, i64)) -> Value {
    json!({
        "id": p.id,
        "kind": p.kind.as_str(),
        "name": p.name,
        "enabled": p.enabled,
        "live": p.live(),
        "position": p.position,
        "client_id": p.client_id,
        "secret_set": p.secret_sealed.is_some(),
        "issuer": p.issuer,
        "new_accounts": p.new_accounts.as_str(),
        "tested_at": p.tested_at,
        "accounts": usage.0,
        "links": usage.1,
        "redirect_url": redirect_uri(state, &p.id),
        "setup_url": super::setup_url(&p.id),
        "needs_keys": p.kind != Kind::Mastodon,
        "custom": p.custom,
    })
}

async fn admin_list(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require(&state, &headers, Permission::SigninManage)?;
    let rows = with_db(&state, |c| {
        store::all_providers(c)?
            .into_iter()
            .map(|p| {
                let usage = store::usage(c, &p.id)?;
                Ok((p, usage))
            })
            .collect::<anyhow::Result<Vec<_>>>()
    })?;
    Ok(Json(json!({
        "key_ok": true,
        "redirect_base": format!("{}/api/auth/", base(&state)),
        "providers": rows.iter().map(|(p, u)| admin_json(&state, p, *u)).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
struct SaveBody {
    name: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    issuer: Option<String>,
    enabled: Option<bool>,
    new_accounts: Option<String>,
}

fn custom_id_ok(id: &str) -> bool {
    id.strip_prefix("oidc-").is_some_and(|slug| {
        !slug.is_empty()
            && slug.len() <= 32
            && slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    })
}

/// Change a provider's settings, or add a community's own OpenID server.
async fn admin_save(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SaveBody>,
) -> ApiResult<Json<Value>> {
    let (_, who) = require(&state, &headers, Permission::SigninManage)?;
    let pid = id.clone();
    let existing = with_db(&state, move |c| {
        Ok((store::provider(c, &pid)?, store::all_providers(c)?))
    })?;
    let mut p = match existing {
        (Some(p), _) => p,
        (None, all) if custom_id_ok(&id) => Provider {
            id: id.clone(),
            kind: Kind::Oidc,
            name: String::new(),
            issuer: None,
            client_id: String::new(),
            secret_sealed: None,
            enabled: false,
            position: all.iter().map(|p| p.position).max().unwrap_or(0) + 1,
            new_accounts: NewAccounts::Anyone,
            tested_at: None,
            custom: true,
        },
        (None, _) => {
            return Err(bad_request(
                "a new provider's id is oidc- and a few lower-case letters or digits",
            ))
        }
    };
    let mut keys_changed = false;
    if let Some(name) = body.name.map(|n| n.trim().to_string()) {
        if name.is_empty() || name.chars().count() > 40 {
            return Err(bad_request("a name of 1 to 40 characters"));
        }
        if p.custom {
            p.name = name;
        }
    }
    if let Some(issuer) = body.issuer.map(|i| i.trim().trim_end_matches('/').to_string()) {
        if p.custom {
            state
                .signin
                .client
                .check_url(&issuer)
                .map_err(|_| bad_request("the issuer is an https:// address"))?;
            keys_changed |= p.issuer.as_deref() != Some(issuer.as_str());
            p.issuer = Some(issuer);
        }
    }
    if let Some(cid) = body.client_id.map(|c| c.trim().to_string()) {
        keys_changed |= cid != p.client_id;
        p.client_id = cid;
    }
    if let Some(secret) = body.client_secret.map(|s| s.trim().to_string()) {
        if !secret.is_empty() {
            p.secret_sealed = Some(state.signin.key.seal(&secret, &p.id).map_err(internal)?);
            keys_changed = true;
        }
    }
    if let Some(n) = body.new_accounts {
        p.new_accounts =
            NewAccounts::parse(&n).ok_or_else(|| bad_request("anyone, members or none"))?;
    }
    if p.custom && (p.name.is_empty() || p.issuer.is_none()) {
        return Err(bad_request("a new OpenID server needs a name and an issuer"));
    }
    if keys_changed {
        // New keys are untested keys: the buttons go until a test passes.
        p.tested_at = None;
        p.enabled = false;
    }
    if let Some(on) = body.enabled {
        if on && !p.has_keys() {
            return Err(conflict("Enter its client id and secret first."));
        }
        if on && p.kind != Kind::Mastodon && p.tested_at.is_none() {
            return Err(conflict("Test it first, so members don't meet a broken button."));
        }
        p.enabled = on;
    }
    let saved = p.clone();
    with_db(&state, move |c| store::save_provider(c, &saved))?;
    tracing::info!(provider = %p.id, by = %who, enabled = p.enabled, "sign-in provider changed");
    let pid = p.id.clone();
    let usage = with_db(&state, move |c| store::usage(c, &pid))?;
    Ok(Json(admin_json(&state, &p, usage)))
}

#[derive(Deserialize)]
struct OrderBody {
    ids: Vec<String>,
}

async fn admin_order(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<OrderBody>,
) -> ApiResult<StatusCode> {
    require(&state, &headers, Permission::SigninManage)?;
    with_db(&state, move |c| store::set_order(c, &body.ids))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn admin_delete(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let (_, who) = require(&state, &headers, Permission::SigninManage)?;
    let pid = id.clone();
    with_db(&state, move |c| store::delete_provider(c, &pid))?;
    tracing::info!(provider = %id, by = %who, "sign-in provider reset");
    Ok(StatusCode::NO_CONTENT)
}

// ── Communities' linked groups ─────────────────────────────────────────────

async fn community_links(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let (principal, _) = require_session(&state, &headers)?;
    let by = principal.to_string();
    let links = with_db(&state, move |c| {
        if let Err(r) = crate::communities::may_moderate(c, id, &by) {
            return Ok(Err(r));
        }
        Ok(Ok(store::links_of(c, id)?))
    })?
    .map_err(refusal)?;
    let links: Vec<Value> = links
        .iter()
        .map(|l| {
            json!({
                "provider": l.provider,
                "provider_name": provider_name(&state, &l.provider),
                "group_id": l.group_id,
                "group_name": l.group_name,
                "linked_by": l.linked_by,
                "linked_at": l.linked_at,
            })
        })
        .collect();
    Ok(Json(json!({ "links": links })))
}

async fn community_unlink(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path((id, provider, group)): Path<(Uuid, String, String)>,
) -> ApiResult<StatusCode> {
    let (principal, _) = require_session(&state, &headers)?;
    let by = principal.to_string();
    let removed = with_db(&state, move |c| {
        if let Err(r) = crate::communities::may_moderate(c, id, &by) {
            return Ok(Err(r));
        }
        Ok(Ok(store::remove_link(c, id, &provider, &group)?))
    })?
    .map_err(refusal)?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError(StatusCode::NOT_FOUND, "no such link".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_path_on_this_site_is_returned_to() {
        assert_eq!(safe_return(Some("/tokens")), "/tokens");
        assert_eq!(safe_return(Some("//evil.example/x")), "/");
        assert_eq!(safe_return(Some("https://evil.example/")), "/");
        assert_eq!(safe_return(Some("/\\evil.example")), "/");
        assert_eq!(safe_return(None), "/");
    }

    #[test]
    fn a_custom_provider_id_is_oidc_and_a_slug() {
        assert!(custom_id_ok("oidc-garage44"));
        assert!(!custom_id_ok("discord"));
        assert!(!custom_id_ok("oidc-"));
        assert!(!custom_id_ok("oidc-Big"));
    }
}
