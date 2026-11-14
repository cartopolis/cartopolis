//! What one account has done, for Admin › Accounts: when it was last here, how
//! long it stayed, and what it made (`docs/mockups/admin-accounts/`).
//!
//! Unlike the rest of `crate::stats` this is about a person, by name, so it is
//! read with [`Permission::AccountsManage`] — the operator who already sees the
//! account's address and can delete it — and only ever about one account at a
//! time, apart from the list's "last seen". Nothing new is recorded for it: the
//! sessions are the ones [`super::Stats`] keeps for [`KEEP_DAYS`], and the
//! counts are rows the world already has.

use super::api::stats_db;
use super::{day_of, now_s, KEEP_DAYS};
use crate::http::{require, with_db, ApiError, ApiResult, HttpState};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use cartopolis_core::permissions::Permission;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::BTreeMap;
use uuid::Uuid;

/// Days the opened line draws as bars.
pub const BAR_DAYS: i64 = 30;

pub fn routes() -> Router<HttpState> {
    Router::new()
        .route("/api/admin/accounts/activity", get(activity))
        .route("/api/admin/accounts/{id}/stats", get(one))
}

/// One line of the list: whether the account is here, and when it last was.
#[derive(Serialize)]
pub struct Seen {
    pub online: bool,
    /// Unix seconds; `None` when it has not been connected in [`KEEP_DAYS`].
    pub last_seen: Option<i64>,
}

/// `GET /api/admin/accounts/activity`: every account's [`Seen`], by id.
async fn activity(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<BTreeMap<String, Seen>>> {
    require(&state, &headers, Permission::AccountsManage)?;
    let online = state.stats.online_accounts();
    let mut out: BTreeMap<String, Seen> = stats_db(&state.stats, |c| {
        let mut q = c.prepare(
            "SELECT account, MAX(COALESCE(ended, last_seen)) FROM sessions GROUP BY account",
        )?;
        let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut out = BTreeMap::new();
        for row in rows {
            let (account, last) = row?;
            out.insert(
                account,
                Seen {
                    online: false,
                    last_seen: Some(last),
                },
            );
        }
        Ok(out)
    })?;
    // A session is written a minute after it starts, so who is here now comes
    // from memory, not from the rows.
    let now = now_s();
    for id in online {
        out.insert(
            id.to_string(),
            Seen {
                online: true,
                last_seen: Some(now),
            },
        );
    }
    Ok(Json(out))
}

/// A sign-in the account has besides its password.
#[derive(Serialize)]
pub struct Identity {
    /// The provider's display name ("Mastodon", "Discord").
    pub provider: String,
    pub handle: String,
}

#[derive(Serialize)]
pub struct Place {
    pub lat: f64,
    pub lng: f64,
}

/// `GET /api/admin/accounts/{id}/stats`.
#[derive(Serialize)]
pub struct AccountStats {
    pub online: bool,
    pub last_seen: Option<i64>,
    /// The app and build of the latest session, as `/ws` named them.
    pub last_app: Option<String>,
    pub last_build: Option<String>,
    /// Sessions in the last [`KEEP_DAYS`] days: each time an app connected.
    pub sign_ins: i64,
    pub keep_days: i64,
    /// Minutes connected per UTC day, the last [`BAR_DAYS`] days, oldest
    /// first; two apps open at once count once.
    pub minutes: Vec<i64>,
    /// Days of those with any time connected.
    pub days_active: i64,
    /// Where the avatar was when it last left the world.
    pub last_place: Option<Place>,
    pub communities: i64,
    pub posts: i64,
    /// Prims it owns: what it built.
    pub objects: i64,
    pub buildings: i64,
    pub contacts: i64,
    /// Chat devices: each app install that holds its keys.
    pub devices: i64,
    pub identities: Vec<Identity>,
}

/// The counts from the world's database.
struct WorldCounts {
    last_place: Option<Place>,
    communities: i64,
    posts: i64,
    objects: i64,
    buildings: i64,
    contacts: i64,
    devices: i64,
    identities: Vec<Identity>,
}

fn world_counts(c: &Connection, id: &str) -> anyhow::Result<Option<WorldCounts>> {
    let Some(place) = c
        .query_row(
            "SELECT last_lat, last_lng FROM accounts WHERE id = ?1",
            [id],
            |r| Ok((r.get::<_, Option<f64>>(0)?, r.get::<_, Option<f64>>(1)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let count = |sql: &str| -> anyhow::Result<i64> { Ok(c.query_row(sql, [id], |r| r.get(0))?) };
    let mut q = c.prepare(
        "SELECT COALESCE(p.name, i.provider), i.handle FROM account_identities i
         LEFT JOIN signin_providers p ON p.id = i.provider
         WHERE i.account = ?1 ORDER BY i.linked_at",
    )?;
    let identities = q
        .query_map([id], |r| {
            Ok(Identity {
                provider: r.get(0)?,
                handle: r.get(1)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(Some(WorldCounts {
        last_place: match place {
            (Some(lat), Some(lng)) => Some(Place { lat, lng }),
            _ => None,
        },
        communities: count("SELECT COUNT(*) FROM community_members WHERE account = ?1")?,
        posts: count("SELECT COUNT(*) FROM posts WHERE author_account = ?1")?,
        objects: count("SELECT COUNT(*) FROM prims WHERE owner = ?1")?,
        buildings: count("SELECT COUNT(*) FROM building_owners WHERE principal = ?1")?,
        contacts: count("SELECT COUNT(*) FROM contacts WHERE account = ?1")?,
        devices: count("SELECT COUNT(*) FROM e2ee_devices WHERE account = ?1")?,
        identities,
    }))
}

/// Minutes per day over `[first_day, first_day + days)`, from sessions as
/// `(start, end)` unix seconds. Overlaps are merged first, so a phone and a
/// browser open together are an hour, not two.
#[must_use]
pub fn minutes_per_day(mut spans: Vec<(i64, i64)>, first_day: i64, days: i64) -> Vec<i64> {
    spans.sort_unstable();
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (s, e) in spans {
        if e <= s {
            continue;
        }
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    let mut secs = vec![0i64; usize::try_from(days).unwrap_or(0)];
    for (s, e) in merged {
        for (i, slot) in secs.iter_mut().enumerate() {
            let day = first_day + i64::try_from(i).unwrap_or(0);
            let (a, b) = (day * 86_400, (day + 1) * 86_400);
            *slot += (e.min(b) - s.max(a)).max(0);
        }
    }
    // A day with any time at all shows at least a minute.
    secs.into_iter().map(|s| (s + 59) / 60).collect()
}

async fn one(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AccountStats>> {
    require(&state, &headers, Permission::AccountsManage)?;
    let key = id.to_string();
    let world = with_db(&state, |c| world_counts(c, &key))?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "no such account".into()))?;
    let online = state.stats.online_accounts().contains(&id);
    let now = now_s();
    let today = day_of(now);
    let first_day = today - BAR_DAYS + 1;
    let key = id.to_string();
    let (last, sign_ins, spans) = stats_db(&state.stats, |c| {
        let last = c
            .query_row(
                "SELECT client, build, COALESCE(ended, last_seen) FROM sessions
                 WHERE account = ?1 ORDER BY started DESC LIMIT 1",
                [&key],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        let sign_ins: i64 = c.query_row(
            "SELECT COUNT(*) FROM sessions WHERE account = ?1 AND started >= ?2",
            params![&key, (today - KEEP_DAYS + 1) * 86_400],
            |r| r.get(0),
        )?;
        let mut q = c.prepare(
            "SELECT started, COALESCE(ended, last_seen) FROM sessions
             WHERE account = ?1 AND COALESCE(ended, last_seen) >= ?2",
        )?;
        let spans = q
            .query_map(params![&key, first_day * 86_400], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok((last, sign_ins, spans))
    })?;
    let minutes = minutes_per_day(spans, first_day, BAR_DAYS);
    let days_active = minutes.iter().filter(|m| **m > 0).count();
    let (last_app, last_build, last_seen) = match last {
        Some((app, build, at)) => (Some(app), (!build.is_empty()).then_some(build), Some(at)),
        None => (None, None, None),
    };
    Ok(Json(AccountStats {
        online,
        last_seen: if online { Some(now) } else { last_seen },
        last_app,
        last_build,
        sign_ins,
        keep_days: KEEP_DAYS,
        minutes,
        days_active: i64::try_from(days_active).unwrap_or(0),
        last_place: world.last_place,
        communities: world.communities,
        posts: world.posts,
        objects: world.objects,
        buildings: world.buildings,
        contacts: world.contacts,
        devices: world.devices,
        identities: world.identities,
    }))
}

#[cfg(test)]
mod tests {
    use super::minutes_per_day;

    #[test]
    fn overlapping_sessions_count_once_and_split_at_midnight() {
        let day = 20_000;
        let t = |d: i64, h: i64| (day + d) * 86_400 + h * 3_600;
        // An hour on the phone with the browser open inside it, then a session
        // from 23:00 to 01:00 across the second midnight.
        let spans = vec![
            (t(0, 9), t(0, 10)),
            (t(0, 9) + 600, t(0, 9) + 1_200),
            (t(1, 23), t(2, 1)),
        ];
        assert_eq!(minutes_per_day(spans, day, 3), vec![60, 60, 60]);
    }

    #[test]
    fn a_session_before_the_window_counts_nothing() {
        assert_eq!(minutes_per_day(vec![(0, 3_600)], 20_000, 2), vec![0, 0]);
    }
}
