//! The stats HTTP surface: the page-view beacon, Admin › Stats, and a
//! community's own numbers for its moderators.
//!
//! Every period is the last `days` days including today, compared with the
//! `days` before it. Numbers about fewer than [`MIN_PEOPLE`] people are left
//! out rather than shown.

use super::{day_of, now_s, web, Stats, KEEP_DAYS, MIN_PEOPLE, SQUARE_PER_LAT, SQUARE_PER_LON};
use crate::http::{internal, require, require_session, with_db, ApiError, ApiResult, HttpState};
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use cartopolis_core::permissions::Permission;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use uuid::Uuid;

pub fn routes() -> Router<HttpState> {
    Router::new()
        .route("/api/stats/hit", post(hit))
        .route("/api/admin/stats/overview", get(overview))
        .route("/api/admin/stats/world", get(world))
        .route("/api/admin/stats/web", get(web_page))
        .route("/api/admin/stats/places", get(places))
        .route("/api/communities/{id}/stats", get(community))
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

/// `POST /api/stats/hit`: one page shown. Anonymous, answers 204 whatever
/// happened, so a page never waits on it or learns anything from it.
///
/// The body is JSON whatever its content type: about.cartopolis.org sends it
/// with `navigator.sendBeacon`, which posts `text/plain` — the one kind of
/// cross-site request a browser sends without asking first, so this endpoint
/// needs no CORS answer at all.
async fn hit(
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> StatusCode {
    let Ok(body) = serde_json::from_slice::<web::HitBody>(&body) else {
        return StatusCode::NO_CONTENT;
    };
    let ip = crate::http::caller_key(&headers, peer);
    if !state.stats_throttle.allow(&ip) {
        return StatusCode::NO_CONTENT;
    }
    let ua = header(&headers, "user-agent");
    let Some(hit) = web::reduce(&body, header(&headers, "accept-language")) else {
        return StatusCode::NO_CONTENT;
    };
    if web::is_bot(ua) {
        state.stats.web_bot(hit.site);
    } else {
        state.stats.web_hit(hit, &ip, ua);
    }
    StatusCode::NO_CONTENT
}

/// Count a visit to a community's public side by somebody who is not in it.
pub(crate) fn community_visit(
    state: &HttpState,
    headers: &HeaderMap,
    peer: SocketAddr,
    community: Uuid,
    caller: Option<&str>,
) {
    let ua = header(headers, "user-agent");
    if web::is_bot(ua) {
        return;
    }
    let member = caller.is_some_and(|c| {
        with_db(state, |conn| {
            Ok(crate::communities::role(conn, community, c).ok().flatten())
        })
        .ok()
        .flatten()
        .is_some()
    });
    if !member {
        let ip = crate::http::caller_key(headers, peer);
        state.stats.community_visit(community, &ip, ua);
    }
}

#[derive(Deserialize)]
struct Period {
    days: Option<i64>,
    site: Option<String>,
}

/// The period asked for: (first day, last day, days), at most two years.
fn period(p: &Period) -> (i64, i64, i64) {
    let days = p.days.unwrap_or(30).clamp(1, 730);
    let today = day_of(now_s());
    (today - days + 1, today, days)
}

fn stats_db<T>(
    stats: &Arc<Stats>,
    f: impl FnOnce(&Connection) -> anyhow::Result<T>,
) -> ApiResult<T> {
    crate::db::blocking(|| {
        let guard = stats
            .conn()
            .lock()
            .map_err(|_| internal(anyhow::anyhow!("stats lock poisoned")))?;
        f(&guard).map_err(internal)
    })
}

/// Different accounts with a session in `[from, to]` (days), per app and in
/// all. Only answerable within [`KEEP_DAYS`].
fn active_between(c: &Connection, from: i64, to: i64) -> anyhow::Result<i64> {
    Ok(c.query_row(
        "SELECT COUNT(DISTINCT account) FROM sessions WHERE started < ?2 AND COALESCE(ended, last_seen) >= ?1",
        params![from * 86_400, (to + 1) * 86_400],
        |r| r.get(0),
    )?)
}

fn sum_web(
    c: &Connection,
    site: Option<&str>,
    dim: &str,
    from: i64,
    to: i64,
) -> anyhow::Result<(i64, i64)> {
    Ok(c.query_row(
        "SELECT COALESCE(SUM(visitors), 0), COALESCE(SUM(views), 0) FROM web_daily \
         WHERE dim = ?1 AND day BETWEEN ?2 AND ?3 AND site != 'community' AND (?4 IS NULL OR site = ?4)",
        params![dim, from, to, site],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

#[derive(Serialize, Default, Clone)]
pub struct DayApps {
    pub day: i64,
    pub all: i64,
    pub android: i64,
    pub desktop: i64,
    pub web: i64,
    pub chat: i64,
    pub unknown: i64,
}

#[derive(Serialize)]
pub struct DayCount {
    pub day: i64,
    pub n: i64,
}

#[derive(Serialize)]
pub struct Funnel {
    pub visitors: i64,
    pub opened_world: i64,
    pub accounts: i64,
    pub signed_in: i64,
    pub came_back: i64,
}

#[derive(Serialize)]
pub struct Place {
    pub lat: f64,
    pub lon: f64,
    pub minutes: i64,
    /// Person-days: a person there on three days counts three times.
    pub people: i64,
}

#[derive(Serialize)]
pub struct Overview {
    pub days: i64,
    pub today: i64,
    pub online_now: usize,
    pub peak_today: i64,
    pub active_today: i64,
    /// `None` past [`KEEP_DAYS`], where sessions are no longer kept.
    pub active_period: Option<i64>,
    pub active_prev: Option<i64>,
    pub accounts_total: i64,
    pub new_accounts: i64,
    pub new_accounts_prev: i64,
    pub visitors: i64,
    pub visitors_prev: i64,
    pub daily: Vec<DayApps>,
    pub visitors_daily: Vec<DayCount>,
    pub funnel: Funnel,
    pub places: Vec<Place>,
}

/// Accounts made per day in `[from, to]`: id and the day.
fn accounts_made(c: &Connection, from: i64, to: i64) -> anyhow::Result<Vec<(String, i64)>> {
    let mut st = c.prepare(
        "SELECT id, CAST(strftime('%s', created_at) AS INTEGER) / 86400 AS d FROM accounts \
         WHERE d BETWEEN ?1 AND ?2",
    )?;
    let rows = st
        .query_map(params![from, to], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

/// The days each account had a session on, within what is kept.
fn session_days(c: &Connection, accounts: &[String]) -> anyhow::Result<HashMap<String, Vec<i64>>> {
    let wanted: HashSet<&str> = accounts.iter().map(String::as_str).collect();
    let mut st = c.prepare(
        "SELECT account, started / 86400, COALESCE(ended, last_seen) / 86400 FROM sessions",
    )?;
    let mut out: HashMap<String, Vec<i64>> = HashMap::new();
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (a, from, to) = row?;
        if wanted.contains(a.as_str()) {
            out.entry(a).or_default().extend(from..=to.min(from + 400));
        }
    }
    for days in out.values_mut() {
        days.sort_unstable();
        days.dedup();
    }
    Ok(out)
}

fn daily_apps(c: &Connection, from: i64, to: i64, today: i64) -> anyhow::Result<Vec<DayApps>> {
    let mut by_day: BTreeMap<i64, DayApps> = (from..=to)
        .map(|day| {
            (
                day,
                DayApps {
                    day,
                    ..Default::default()
                },
            )
        })
        .collect();
    let mut put = |day: i64, client: &str, n: i64| {
        if let Some(d) = by_day.get_mut(&day) {
            match client {
                "all" => d.all = n,
                "android" => d.android = n,
                "desktop" => d.desktop = n,
                "web" => d.web = n,
                "chat" => d.chat = n,
                _ => d.unknown += n,
            }
        }
    };
    let mut st =
        c.prepare("SELECT day, client, people FROM active_daily WHERE day BETWEEN ?1 AND ?2")?;
    for row in st.query_map(params![from, to], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })? {
        let (day, client, n) = row?;
        put(day, &client, n);
    }
    // Today has not been closed yet: count it from the sessions.
    let mut st = c.prepare(
        "SELECT client, COUNT(DISTINCT account) FROM sessions WHERE COALESCE(ended, last_seen) >= ?1 GROUP BY client",
    )?;
    for row in st.query_map([today * 86_400], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })? {
        let (client, n) = row?;
        put(today, &client, n);
    }
    put(today, "all", active_between(c, today, today)?);
    Ok(by_day.into_values().collect())
}

fn top_places(c: &Connection, from: i64, to: i64, limit: i64) -> anyhow::Result<Vec<Place>> {
    let mut st = c.prepare(
        "SELECT sx, sy, SUM(minutes), SUM(people) FROM squares_daily \
         WHERE day BETWEEN ?1 AND ?2 AND people >= ?3 GROUP BY sx, sy ORDER BY 3 DESC LIMIT ?4",
    )?;
    let rows = st
        .query_map(params![from, to, MIN_PEOPLE, limit], |r| {
            let (sx, sy): (i64, i64) = (r.get(0)?, r.get(1)?);
            #[allow(clippy::cast_precision_loss)]
            Ok(Place {
                lat: (sy as f64 + 0.5) / SQUARE_PER_LAT,
                lon: (sx as f64 + 0.5) / SQUARE_PER_LON,
                minutes: r.get(2)?,
                people: r.get(3)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

async fn overview(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Query(q): Query<Period>,
) -> ApiResult<Json<Overview>> {
    require(&state, &headers, Permission::StatsView)?;
    let (from, to, days) = period(&q);
    let today = to;
    let (accounts_total, made, made_prev) = with_db(&state, |c| {
        let total: i64 = c.query_row(
            "SELECT COUNT(*) FROM accounts WHERE disabled = 0",
            [],
            |r| r.get(0),
        )?;
        Ok((
            total,
            accounts_made(c, from, to)?,
            accounts_made(c, from - days, from - 1)?.len(),
        ))
    })?;
    let online_now = state.stats.online();
    let o = stats_db(&state.stats, |c| {
        let kept = days <= KEEP_DAYS;
        let active_period = kept.then(|| active_between(c, from, to)).transpose()?;
        let active_prev = (2 * days <= KEEP_DAYS)
            .then(|| active_between(c, from - days, from - 1))
            .transpose()?;
        let peak_today: i64 = c.query_row(
            "SELECT COALESCE(MAX(peak), 0) FROM online_hourly WHERE hour >= ?1",
            [today * 24],
            |r| r.get(0),
        )?;
        let (visitors, _) = sum_web(c, None, "total", from, to)?;
        let (visitors_prev, _) = sum_web(c, None, "total", from - days, from - 1)?;
        let (opened_world, _) = sum_web(c, Some("viewer"), "total", from, to)?;
        let mut st = c.prepare(
            "SELECT day, SUM(visitors) FROM web_daily WHERE dim = 'total' AND site != 'community' \
             AND day BETWEEN ?1 AND ?2 GROUP BY day",
        )?;
        let got: HashMap<i64, i64> = st
            .query_map(params![from, to], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let visitors_daily = (from..=to)
            .map(|day| DayCount {
                day,
                n: got.get(&day).copied().unwrap_or(0),
            })
            .collect();
        let ids: Vec<String> = made.iter().map(|(id, _)| id.clone()).collect();
        let seen = session_days(c, &ids)?;
        let signed_in = i64::try_from(seen.len()).unwrap_or(0);
        // Came back: a session on a later day, within a week of the first.
        let came_back = seen
            .values()
            .filter(|d| {
                d.first()
                    .is_some_and(|&f| d.iter().any(|&x| x > f && x <= f + 7))
            })
            .count();
        Ok(Overview {
            days,
            today,
            online_now,
            peak_today,
            active_today: active_between(c, today, today)?,
            active_period,
            active_prev,
            accounts_total,
            new_accounts: i64::try_from(made.len()).unwrap_or(0),
            new_accounts_prev: i64::try_from(made_prev).unwrap_or(0),
            visitors,
            visitors_prev,
            daily: daily_apps(c, from, to, today)?,
            visitors_daily,
            funnel: Funnel {
                visitors,
                opened_world,
                accounts: i64::try_from(made.len()).unwrap_or(0),
                signed_in,
                came_back: i64::try_from(came_back).unwrap_or(0),
            },
            places: top_places(c, from, to, 5)?,
        })
    })?;
    Ok(Json(o))
}

#[derive(Serialize)]
pub struct EventTotal {
    pub kind: String,
    pub n: i64,
    pub prev: i64,
    /// Per day, oldest first, for the sparkline.
    pub daily: Vec<i64>,
}

#[derive(Serialize)]
pub struct HourAvg {
    pub hour: i64,
    pub avg: f64,
    pub peak: i64,
}

#[derive(Serialize)]
pub struct Cohort {
    /// The Monday (a day number) of the week the accounts were made in.
    pub week: i64,
    pub size: i64,
    /// Share seen again in each following week, while that week has begun.
    /// Empty for a week with fewer than [`MIN_PEOPLE`] new accounts.
    pub back: Vec<f64>,
}

#[derive(Serialize)]
pub struct AppUse {
    pub client: String,
    pub build: String,
    pub people: i64,
}

#[derive(Serialize)]
pub struct Refusal {
    pub reason: String,
    pub protocol: i64,
    pub client: String,
    pub build: String,
    pub attempts: i64,
    pub devices: i64,
}

#[derive(Serialize)]
pub struct World {
    pub days: i64,
    pub sessions: i64,
    pub sessions_prev: i64,
    pub median_minutes: i64,
    pub longest_minutes: i64,
    pub peak: i64,
    pub peak_hour: Option<i64>,
    pub events: Vec<EventTotal>,
    pub hours: Vec<HourAvg>,
    pub cohorts: Vec<Cohort>,
    pub apps: Vec<AppUse>,
    pub refused: Vec<Refusal>,
}

fn monday(day: i64) -> i64 {
    // Day 0 (1 January 1970) was a Thursday.
    day - (day + 3).rem_euclid(7)
}

fn events_between(
    c: &Connection,
    community: Option<&str>,
    from: i64,
    to: i64,
    days: i64,
) -> anyhow::Result<Vec<EventTotal>> {
    let mut totals: BTreeMap<String, EventTotal> = BTreeMap::new();
    let mut st = c.prepare(
        "SELECT kind, day, SUM(n) FROM events_daily WHERE day BETWEEN ?1 AND ?2 \
         AND (?3 IS NULL OR community = ?3) GROUP BY kind, day",
    )?;
    let len = usize::try_from(days).unwrap_or(0);
    for row in st.query_map(params![from - days, to, community], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })? {
        let (kind, day, n) = row?;
        let t = totals.entry(kind.clone()).or_insert_with(|| EventTotal {
            kind,
            n: 0,
            prev: 0,
            daily: vec![0; len],
        });
        if day < from {
            t.prev += n;
        } else {
            t.n += n;
            if let Some(slot) = usize::try_from(day - from)
                .ok()
                .and_then(|i| t.daily.get_mut(i))
            {
                *slot += n;
            }
        }
    }
    Ok(totals.into_values().collect())
}

async fn world(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Query(q): Query<Period>,
) -> ApiResult<Json<World>> {
    require(&state, &headers, Permission::StatsView)?;
    let (from, to, days) = period(&q);
    let today = to;
    let made = with_db(&state, |c| accounts_made(c, monday(today) - 35, today))?;
    let w = stats_db(&state.stats, |c| {
        let count = |a: i64, b: i64| -> anyhow::Result<i64> {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM sessions WHERE started BETWEEN ?1 AND ?2",
                params![a * 86_400, (b + 1) * 86_400 - 1],
                |r| r.get(0),
            )?)
        };
        let mut st = c.prepare(
            "SELECT (COALESCE(ended, last_seen) - started) / 60 FROM sessions WHERE started >= ?1 ORDER BY 1",
        )?;
        let lengths: Vec<i64> = st
            .query_map([from * 86_400], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let (peak, peak_hour): (i64, Option<i64>) = c
            .query_row(
                "SELECT peak, hour FROM online_hourly WHERE hour BETWEEN ?1 AND ?2 ORDER BY peak DESC, hour DESC LIMIT 1",
                params![from * 24, (to + 1) * 24 - 1],
                |r| Ok((r.get(0)?, Some(r.get(1)?))),
            )
            .optional()?
            .unwrap_or((0, None));
        let mut st = c.prepare(
            "SELECT hour, sum, samples, peak FROM online_hourly WHERE hour BETWEEN ?1 AND ?2",
        )?;
        let hours = st
            .query_map(params![from * 24, (to + 1) * 24 - 1], |r| {
                let (sum, samples): (i64, i64) = (r.get(1)?, r.get(2)?);
                #[allow(clippy::cast_precision_loss)]
                Ok(HourAvg {
                    hour: r.get(0)?,
                    avg: sum as f64 / samples.max(1) as f64,
                    peak: r.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        // Who comes back, for the last six weeks' new accounts.
        let ids: Vec<String> = made.iter().map(|(id, _)| id.clone()).collect();
        let seen = session_days(c, &ids)?;
        let mut weeks: BTreeMap<i64, Vec<&String>> = BTreeMap::new();
        for (id, d) in &made {
            weeks.entry(monday(*d)).or_default().push(id);
        }
        let cohorts = weeks
            .into_iter()
            .map(|(week, ids)| {
                let size = i64::try_from(ids.len()).unwrap_or(0);
                let mut back = Vec::new();
                if size >= MIN_PEOPLE {
                    let mut k = 1;
                    while week + 7 * k <= today {
                        let (a, b) = (week + 7 * k, week + 7 * k + 6);
                        let n = ids
                            .iter()
                            .filter(|id| {
                                seen.get(id.as_str())
                                    .is_some_and(|d| d.iter().any(|&x| x >= a && x <= b))
                            })
                            .count();
                        #[allow(clippy::cast_precision_loss)]
                        back.push(n as f64 / size as f64);
                        k += 1;
                    }
                }
                Cohort { week, size, back }
            })
            .collect();
        let mut st = c.prepare(
            "SELECT client, build, COUNT(DISTINCT account) FROM sessions WHERE started >= ?1 \
             GROUP BY client, build ORDER BY 3 DESC",
        )?;
        let apps = st
            .query_map([(today - 6) * 86_400], |r| {
                Ok(AppUse {
                    client: r.get(0)?,
                    build: r.get(1)?,
                    people: r.get(2)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        let mut st = c.prepare(
            "SELECT reason, protocol, client, build, SUM(attempts), MAX(devices) FROM refused_daily \
             WHERE day >= ?1 GROUP BY reason, protocol, client, build ORDER BY 5 DESC",
        )?;
        let refused = st
            .query_map([today - 6], |r| {
                Ok(Refusal {
                    reason: r.get(0)?,
                    protocol: r.get(1)?,
                    client: r.get(2)?,
                    build: r.get(3)?,
                    attempts: r.get(4)?,
                    devices: r.get(5)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(World {
            days,
            sessions: count(from, to)?,
            sessions_prev: count(from - days, from - 1)?,
            median_minutes: lengths.get(lengths.len() / 2).copied().unwrap_or(0),
            longest_minutes: lengths.last().copied().unwrap_or(0),
            peak,
            peak_hour,
            events: events_between(c, None, from, to, days)?,
            hours,
            cohorts,
            apps,
            refused,
        })
    })?;
    Ok(Json(w))
}

#[derive(Serialize)]
pub struct Ranked {
    pub value: String,
    pub visitors: i64,
    pub views: i64,
}

#[derive(Serialize)]
pub struct WebDay {
    pub day: i64,
    pub visitors: i64,
    pub views: i64,
}

#[derive(Serialize)]
pub struct Web {
    pub days: i64,
    pub site: Option<String>,
    pub visitors: i64,
    pub visitors_prev: i64,
    pub views: i64,
    pub opened_world: i64,
    pub bots: i64,
    pub daily: Vec<WebDay>,
    pub pages: Vec<Ranked>,
    pub sources: Vec<Ranked>,
    pub devices: Vec<Ranked>,
    pub languages: Vec<Ranked>,
    /// Two-letter codes, or `unknown` while the country table is not loaded.
    pub countries: Vec<Ranked>,
    /// Whole APK downloads, as Caddy reports them (`stats::apk`), whatever site
    /// is chosen: the APK is on none of them.
    pub apk_downloads: i64,
    pub apk_downloads_prev: i64,
}

fn ranked(
    c: &Connection,
    site: Option<&str>,
    dim: &str,
    from: i64,
    to: i64,
) -> anyhow::Result<Vec<Ranked>> {
    let mut st = c.prepare(
        // A page is named with its site unless it is cartopolis.org's own, so
        // the about site's `/` is not counted as the front page's.
        "SELECT CASE WHEN dim = 'path' AND site != 'web' THEN site || ' ' || value ELSE value END AS label, \
         SUM(visitors), SUM(views) FROM web_daily WHERE dim = ?1 AND day BETWEEN ?2 AND ?3 \
         AND site != 'community' AND (?4 IS NULL OR site = ?4) \
         GROUP BY label ORDER BY 2 DESC LIMIT 20",
    )?;
    let rows = st
        .query_map(params![dim, from, to, site], |r| {
            Ok(Ranked {
                value: r.get(0)?,
                visitors: r.get(1)?,
                views: r.get(2)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

async fn web_page(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Query(q): Query<Period>,
) -> ApiResult<Json<Web>> {
    require(&state, &headers, Permission::StatsView)?;
    let (from, to, days) = period(&q);
    let site = q.site.filter(|s| web::SITES.contains(&s.as_str()));
    let s = site.as_deref();
    let w = stats_db(&state.stats, |c| {
        let (visitors, views) = sum_web(c, s, "total", from, to)?;
        let (visitors_prev, _) = sum_web(c, s, "total", from - days, from - 1)?;
        let (opened_world, _) = sum_web(c, Some("viewer"), "total", from, to)?;
        let (_, bots) = sum_web(c, s, "bots", from, to)?;
        let apk = |a: i64, b: i64| -> anyhow::Result<i64> {
            Ok(c.query_row(
                "SELECT COALESCE(SUM(n), 0) FROM events_daily WHERE kind = 'apk_download' AND day BETWEEN ?1 AND ?2",
                params![a, b],
                |r| r.get(0),
            )?)
        };
        let mut st = c.prepare(
            "SELECT day, SUM(visitors), SUM(views) FROM web_daily WHERE dim = 'total' AND site != 'community' \
             AND day BETWEEN ?1 AND ?2 AND (?3 IS NULL OR site = ?3) GROUP BY day",
        )?;
        let got: HashMap<i64, (i64, i64)> = st
            .query_map(params![from, to, s], |r| {
                Ok((r.get(0)?, (r.get(1)?, r.get(2)?)))
            })?
            .collect::<Result<_, _>>()?;
        Ok(Web {
            days,
            site: site.clone(),
            visitors,
            visitors_prev,
            views,
            opened_world,
            bots,
            daily: (from..=to)
                .map(|day| {
                    let (visitors, views) = got.get(&day).copied().unwrap_or((0, 0));
                    WebDay {
                        day,
                        visitors,
                        views,
                    }
                })
                .collect(),
            pages: ranked(c, s, "path", from, to)?,
            sources: ranked(c, s, "source", from, to)?,
            devices: ranked(c, s, "device", from, to)?,
            languages: ranked(c, s, "lang", from, to)?,
            countries: ranked(c, s, "country", from, to)?,
            apk_downloads: apk(from, to)?,
            apk_downloads_prev: apk(from - days, from - 1)?,
        })
    })?;
    Ok(Json(w))
}

#[derive(Serialize)]
pub struct Square {
    pub sx: i64,
    pub sy: i64,
    pub lat: f64,
    pub lon: f64,
    pub minutes: i64,
    pub people: i64,
}

#[derive(Serialize)]
pub struct Places {
    pub days: i64,
    /// Degrees per square, so a page can draw them: 1/this.
    pub per_lat: f64,
    pub per_lon: f64,
    pub squares: Vec<Square>,
}

async fn places(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Query(q): Query<Period>,
) -> ApiResult<Json<Places>> {
    require(&state, &headers, Permission::StatsView)?;
    let (from, to, days) = period(&q);
    let squares = stats_db(&state.stats, |c| {
        let mut st = c.prepare(
            "SELECT sx, sy, SUM(minutes), SUM(people) FROM squares_daily \
             WHERE day BETWEEN ?1 AND ?2 AND people >= ?3 GROUP BY sx, sy ORDER BY 3 DESC LIMIT 2000",
        )?;
        let rows = st
            .query_map(params![from, to, MIN_PEOPLE], |r| {
                let (sx, sy): (i64, i64) = (r.get(0)?, r.get(1)?);
                #[allow(clippy::cast_precision_loss)]
                Ok(Square {
                    sx,
                    sy,
                    lat: (sy as f64 + 0.5) / SQUARE_PER_LAT,
                    lon: (sx as f64 + 0.5) / SQUARE_PER_LON,
                    minutes: r.get(2)?,
                    people: r.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    })?;
    Ok(Json(Places {
        days,
        per_lat: SQUARE_PER_LAT,
        per_lon: SQUARE_PER_LON,
        squares,
    }))
}

#[derive(Serialize)]
pub struct TopicUse {
    pub topic: String,
    pub name: String,
    pub messages: i64,
    pub messages_prev: i64,
    /// `None` when fewer than [`MIN_PEOPLE`] people wrote.
    pub writers: Option<i64>,
}

#[derive(Serialize)]
pub struct WeekJoins {
    pub week: i64,
    pub joined: i64,
    pub left: i64,
}

#[derive(Serialize)]
pub struct ServerCount {
    pub server: String,
    pub n: i64,
}

#[derive(Serialize)]
pub struct CommunityStats {
    pub days: i64,
    pub members: i64,
    pub joined: i64,
    pub left: i64,
    pub weeks: Vec<WeekJoins>,
    /// `None` when fewer than [`MIN_PEOPLE`].
    pub active_members: Option<i64>,
    pub messages: i64,
    pub messages_prev: i64,
    pub topics: Vec<TopicUse>,
    pub call_minutes: i64,
    pub posts: i64,
    pub replies_in: i64,
    pub boosts_in: i64,
    pub followers: i64,
    pub followers_new: i64,
    pub followers_by_server: Vec<ServerCount>,
    pub post_places: Vec<Place>,
    pub visitors: i64,
    pub visitors_prev: i64,
    pub visitors_daily: Vec<DayCount>,
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

async fn community(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<Period>,
) -> ApiResult<Json<CommunityStats>> {
    let (principal, _) = require_session(&state, &headers)?;
    let (from, to, days) = period(&q);
    let cid = id.to_string();
    let (from_ms, to_ms, prev_ms) = (
        from * 86_400_000,
        (to + 1) * 86_400_000,
        (from - days) * 86_400_000,
    );
    struct FromWorld {
        members: i64,
        joined_days: Vec<i64>,
        topics: Vec<(String, String)>,
        posts: i64,
        replies_in: i64,
        boosts_in: i64,
        followers: Vec<(String, i64)>,
        post_places: Vec<Place>,
    }
    let who = principal.to_string();
    let role = with_db(&state, |c| {
        Ok(crate::communities::role(c, id, &who).ok().flatten())
    })?;
    if !role.is_some_and(|r| r.moderates()) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "only the community's owner and moderators see its numbers".into(),
        ));
    }
    let world = with_db(&state, |c| {
        let members: i64 = c.query_row(
            "SELECT COUNT(*) FROM community_members WHERE community = ?1",
            [&cid],
            |r| r.get(0),
        )?;
        let mut st = c.prepare("SELECT joined_ms / 86400000 FROM community_members WHERE community = ?1 AND joined_ms >= ?2")?;
        let joined_days = st
            .query_map(params![cid, prev_ms.min((to - 83) * 86_400_000)], |r| {
                r.get(0)
            })?
            .collect::<Result<_, _>>()?;
        let mut st =
            c.prepare("SELECT group_id, name FROM topics WHERE community = ?1 ORDER BY position")?;
        let topics = st
            .query_map([&cid], |r| Ok((hex(&r.get::<_, Vec<u8>>(0)?), r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let count = |sql: &str| -> anyhow::Result<i64> {
            Ok(c.query_row(sql, params![cid, from_ms, to_ms], |r| r.get(0))?)
        };
        let posts = count("SELECT COUNT(*) FROM posts WHERE community = ?1 AND author_account IS NOT NULL AND parent IS NULL AND created_ms BETWEEN ?2 AND ?3")?;
        let replies_in = count("SELECT COUNT(*) FROM posts WHERE community = ?1 AND author_actor IS NOT NULL AND via_actor IS NULL AND created_ms BETWEEN ?2 AND ?3")?;
        let boosts_in = count("SELECT COUNT(*) FROM posts WHERE community = ?1 AND via_actor IS NOT NULL AND created_ms BETWEEN ?2 AND ?3")?;
        let mut st = c.prepare("SELECT actor, followed_ms FROM fedi_followers WHERE local = ?1")?;
        let followers = st
            .query_map([crate::fediverse::posts::group_local(id)], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<Result<_, _>>()?;
        let mut st = c.prepare(
            "SELECT ROUND(lat, 3), ROUND(lng, 3), COUNT(*) FROM posts WHERE community = ?1 AND author_account IS NOT NULL \
             AND lat IS NOT NULL AND created_ms BETWEEN ?2 AND ?3 GROUP BY 1, 2 ORDER BY 3 DESC LIMIT 5",
        )?;
        let post_places = st
            .query_map(params![cid, from_ms, to_ms], |r| {
                Ok(Place {
                    lat: r.get(0)?,
                    lon: r.get(1)?,
                    minutes: 0,
                    people: r.get(2)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(FromWorld {
            members,
            joined_days,
            topics,
            posts,
            replies_in,
            boosts_in,
            followers,
            post_places,
        })
    })?;
    let out = stats_db(&state.stats, |c| {
        let events = events_between(c, Some(&cid), from, to, days)?;
        let n = |k: &str| {
            events
                .iter()
                .find(|e| e.kind == k)
                .map_or((0, 0), |e| (e.n, e.prev))
        };
        let (messages, messages_prev) = n("community_message");
        let (left, _) = n("community_left");
        let (call_minutes, _) = n("call_minutes");
        let mut st = c.prepare(
            "SELECT topic, day >= ?2, SUM(n) FROM events_daily WHERE community = ?1 AND kind = 'community_message' \
             AND day BETWEEN ?3 AND ?4 AND topic != '' GROUP BY 1, 2",
        )?;
        let mut per_topic: HashMap<String, (i64, i64)> = HashMap::new();
        for row in st.query_map(params![cid, from, from - days, to], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, bool>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })? {
            let (t, current, n) = row?;
            let e = per_topic.entry(t).or_default();
            if current {
                e.0 += n
            } else {
                e.1 += n
            }
        }
        let mut st = c.prepare(
            "SELECT topic, COUNT(DISTINCT account) FROM activity WHERE community = ?1 AND topic != '' AND day BETWEEN ?2 AND ?3 GROUP BY topic",
        )?;
        let writers: HashMap<String, i64> = st
            .query_map(params![cid, from, to], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let topics = world
            .topics
            .iter()
            .map(|(topic, name)| {
                let (messages, messages_prev) = per_topic.get(topic).copied().unwrap_or((0, 0));
                let w = writers.get(topic).copied().unwrap_or(0);
                TopicUse {
                    topic: topic.clone(),
                    name: name.clone(),
                    messages: if w >= MIN_PEOPLE { messages } else { 0 },
                    messages_prev: if w >= MIN_PEOPLE { messages_prev } else { 0 },
                    writers: (w >= MIN_PEOPLE).then_some(w),
                }
            })
            .collect();
        let active: i64 = c.query_row(
            "SELECT COUNT(DISTINCT account) FROM activity WHERE community = ?1 AND day BETWEEN ?2 AND ?3",
            params![cid, from, to],
            |r| r.get(0),
        )?;
        let mut st = c.prepare(
            "SELECT day, community_left FROM (SELECT day, SUM(n) AS community_left FROM events_daily \
             WHERE community = ?1 AND kind = 'community_left' AND day >= ?2 GROUP BY day)",
        )?;
        let left_days: Vec<(i64, i64)> = st
            .query_map(params![cid, monday(to) - 77], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<Result<_, _>>()?;
        let weeks = (0..12)
            .map(|k| {
                let week = monday(to) - 7 * (11 - k);
                let joined = world
                    .joined_days
                    .iter()
                    .filter(|&&d| monday(d) == week)
                    .count();
                let left = left_days
                    .iter()
                    .filter(|(d, _)| monday(*d) == week)
                    .map(|(_, n)| n)
                    .sum();
                WeekJoins {
                    week,
                    joined: i64::try_from(joined).unwrap_or(0),
                    left,
                }
            })
            .collect();
        let visit = |a: i64, b: i64| -> anyhow::Result<i64> {
            Ok(c.query_row(
                "SELECT COALESCE(SUM(visitors), 0) FROM web_daily WHERE site = 'community' AND dim = 'visit' AND value = ?1 AND day BETWEEN ?2 AND ?3",
                params![cid, a, b],
                |r| r.get(0),
            )?)
        };
        let mut st = c.prepare(
            "SELECT day, visitors FROM web_daily WHERE site = 'community' AND dim = 'visit' AND value = ?1 AND day BETWEEN ?2 AND ?3",
        )?;
        let got: HashMap<i64, i64> = st
            .query_map(params![cid, from, to], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut by_server: BTreeMap<String, i64> = BTreeMap::new();
        for (actor, _) in &world.followers {
            let host = url::Url::parse(actor)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_else(|| "unknown".into());
            *by_server.entry(host).or_default() += 1;
        }
        let mut followers_by_server: Vec<ServerCount> = by_server
            .into_iter()
            .map(|(server, n)| ServerCount { server, n })
            .collect();
        followers_by_server.sort_by(|a, b| b.n.cmp(&a.n));
        Ok(CommunityStats {
            days,
            members: world.members,
            joined: i64::try_from(world.joined_days.iter().filter(|&&d| d >= from).count())
                .unwrap_or(0),
            left,
            weeks,
            active_members: (active >= MIN_PEOPLE).then_some(active),
            messages,
            messages_prev,
            topics,
            call_minutes,
            posts: world.posts,
            replies_in: world.replies_in,
            boosts_in: world.boosts_in,
            followers: i64::try_from(world.followers.len()).unwrap_or(0),
            followers_new: i64::try_from(
                world
                    .followers
                    .iter()
                    .filter(|(_, ms)| *ms >= from_ms)
                    .count(),
            )
            .unwrap_or(0),
            followers_by_server,
            post_places: world.post_places,
            visitors: visit(from, to)?,
            visitors_prev: visit(from - days, from - 1)?,
            visitors_daily: (from..=to)
                .map(|day| DayCount {
                    day,
                    n: got.get(&day).copied().unwrap_or(0),
                })
                .collect(),
        })
    })?;
    Ok(Json(out))
}
