//! Usage statistics: how the world is used, and who visits the sites.
//!
//! Counts, not people (`docs/mockups/stats/`). Everything is tallied in memory
//! as it happens and written to `stats.db` — its own file beside `world.db`,
//! with its own migrations (`migrations_stats/`) — once a minute by [`spawn`].
//! Recording never touches the disk or awaits, so a counter on the tick's path
//! costs a mutex and a hash insert.
//!
//! What is kept, and for how long:
//! - **Sessions** per account for [`KEEP_DAYS`], then only the daily totals in
//!   `active_daily` — what "came back" and "active in 30 days" need.
//! - **Web visits** only as daily totals. A visitor is a hash of a secret made
//!   at midnight, the address, the browser and the site; the secret is never
//!   written, so no visit can be joined to the next day's, and the address goes
//!   no further than the hash.
//! - **Squares**: minutes per 250 m square and how many people that day; a
//!   closed day keeps only squares with at least [`MIN_PEOPLE`].
//!
//! Days are UTC: an hour row is a unix hour, so the page draws them in its
//! reader's zone, and a restart mid-day only makes that day's visitor counts a
//! little low (the in-memory sets start over, and the stored count is the
//! larger of the two).

pub mod api;
mod store;
pub mod web;

use crate::db::SharedConn;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// How long anything per account is kept.
pub const KEEP_DAYS: i64 = 90;
/// Below this many different people, a square, a topic or a cohort shows no
/// numbers.
pub const MIN_PEOPLE: i64 = 5;
/// Squares are 1/444° of latitude by 1/266° of longitude: 250 m by 250 m at
/// Groningen's latitude, narrower further north.
pub const SQUARE_PER_LAT: f64 = 444.0;
pub const SQUARE_PER_LON: f64 = 266.0;
/// Distinct values one (site, dimension) may gain in a day before the rest are
/// counted as `(other)`: a script cannot fill the table with made-up pages.
const MAX_VALUES: usize = 300;

#[must_use]
pub fn now_s() -> i64 {
    i64::try_from(crate::auth::unix_now_s()).unwrap_or(0)
}

#[must_use]
pub const fn day_of(unix_s: i64) -> i64 {
    unix_s.div_euclid(86_400)
}

/// The square a point falls in.
#[must_use]
pub fn square_of(lat: f64, lon: f64) -> (i64, i64) {
    #[allow(clippy::cast_possible_truncation)]
    let sq = |v: f64, per: f64| (v * per).floor() as i64;
    (sq(lon, SQUARE_PER_LON), sq(lat, SQUARE_PER_LAT))
}

/// Which app opened a session, from the `client` query parameter on `/ws`.
/// Apps older than the parameter, and plain TCP, are `unknown`.
#[must_use]
pub fn client_tag(raw: Option<&str>) -> &'static str {
    match raw {
        Some("android") => "android",
        Some("desktop") => "desktop",
        Some("web") => "web",
        Some("chat") => "chat",
        _ => "unknown",
    }
}

/// A build id as an app reports it: a short commit, nothing longer or stranger.
#[must_use]
pub fn build_tag(raw: Option<&str>) -> String {
    raw.unwrap_or("")
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect()
}

/// Something the server passed on, counted per day. The names are the stored
/// `kind`s, so they never change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Event {
    /// A sealed message in a private chat or group.
    PrivateMessage,
    /// A sealed message in a community's topic.
    CommunityMessage,
    /// A message to people nearby.
    HereMessage,
    /// A message to everybody on the server.
    EveryoneMessage,
    CallStarted,
    /// Minutes one person spent in a call, counted when they leave it.
    CallMinutes,
    PostPublished,
    /// A reply, boost or follow that arrived from another server.
    ReplyIn,
    BoostIn,
    FollowIn,
    PrimCreated,
    PrimEdited,
    CommunityJoined,
    CommunityLeft,
}

impl Event {
    #[must_use]
    pub const fn kind(self) -> &'static str {
        match self {
            Self::PrivateMessage => "private_message",
            Self::CommunityMessage => "community_message",
            Self::HereMessage => "here_message",
            Self::EveryoneMessage => "everyone_message",
            Self::CallStarted => "call_started",
            Self::CallMinutes => "call_minutes",
            Self::PostPublished => "post_published",
            Self::ReplyIn => "reply_in",
            Self::BoostIn => "boost_in",
            Self::FollowIn => "follow_in",
            Self::PrimCreated => "prim_created",
            Self::PrimEdited => "prim_edited",
            Self::CommunityJoined => "community_joined",
            Self::CommunityLeft => "community_left",
        }
    }
}

/// A day's distinct set and the views added since the last write.
#[derive(Default)]
struct Tally {
    seen: HashSet<u64>,
    views: i64,
    dirty: bool,
}

#[derive(Default)]
struct Square {
    minutes: i64,
    /// Names, in memory only: the stored row holds the count.
    people: HashSet<String>,
}

enum SessionOp {
    Start {
        id: Uuid,
        account: Uuid,
        client: &'static str,
        build: String,
        protocol: u16,
        at: i64,
    },
    End {
        id: Uuid,
        at: i64,
    },
}

type RefusedKey = (&'static str, u16, &'static str, String);
type WebKey = (&'static str, &'static str, String);
type EventKey = (&'static str, String, String);

struct Live {
    day: i64,
    secret: [u8; 32],
    /// Open sessions: session id → account.
    open: HashMap<Uuid, Uuid>,
    sessions: Vec<SessionOp>,
    web: HashMap<WebKey, Tally>,
    /// How many distinct values each (site, dimension) holds today.
    values: HashMap<(&'static str, &'static str), usize>,
    events: HashMap<EventKey, i64>,
    /// What [`Stats::event_once`] has already counted today.
    once: HashSet<(&'static str, String)>,
    activity_seen: HashSet<(Uuid, String, String)>,
    activity_new: Vec<(Uuid, String, String)>,
    squares: HashMap<(i64, i64), Square>,
    refused: HashMap<RefusedKey, Tally>,
    /// The current hour's online samples: (hour, sum, samples, peak).
    online: Option<(i64, i64, i64, i64)>,
}

impl Live {
    fn new(day: i64) -> Self {
        Self {
            day,
            secret: fresh_secret(),
            open: HashMap::new(),
            sessions: Vec::new(),
            web: HashMap::new(),
            values: HashMap::new(),
            events: HashMap::new(),
            once: HashSet::new(),
            activity_seen: HashSet::new(),
            activity_new: Vec::new(),
            squares: HashMap::new(),
            refused: HashMap::new(),
            online: None,
        }
    }

    fn visitor(&self, ip: &str, user_agent: &str, scope: &str) -> u64 {
        use blake2::{Blake2s256, Digest};
        let mut h = Blake2s256::new();
        h.update(self.secret);
        for part in [ip, user_agent, scope] {
            h.update((part.len() as u64).to_le_bytes());
            h.update(part.as_bytes());
        }
        let d = h.finalize();
        u64::from_le_bytes(d[..8].try_into().unwrap_or_default())
    }

    fn web(&mut self, site: &'static str, dim: &'static str, value: String, who: u64) {
        let key = (site, dim, value);
        let key = if self.web.contains_key(&key) {
            key
        } else {
            let n = self.values.entry((site, dim)).or_default();
            if *n >= MAX_VALUES {
                (site, dim, "(other)".to_string())
            } else {
                *n += 1;
                key
            }
        };
        let t = self.web.entry(key).or_default();
        t.seen.insert(who);
        t.views += 1;
        t.dirty = true;
    }
}

fn fresh_secret() -> [u8; 32] {
    let mut s = [0u8; 32];
    if getrandom::getrandom(&mut s).is_err() {
        // Not secret, but still not stored: the hash then only fails to hide
        // an address from someone who reads this process's memory.
        s[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        s[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    }
    s
}

/// The counters and their database. One per server, shared by every task.
pub struct Stats {
    conn: SharedConn,
    live: Mutex<Live>,
}

impl Stats {
    /// `stats.db` beside the world's database, or a database in memory for a
    /// server whose world is in memory.
    pub fn open_beside(database_path: &str) -> anyhow::Result<Arc<Self>> {
        let path = if database_path == ":memory:" {
            ":memory:".to_string()
        } else {
            std::path::Path::new(database_path)
                .with_file_name("stats.db")
                .to_string_lossy()
                .into_owned()
        };
        Self::open(&path)
    }

    pub fn open(path: &str) -> anyhow::Result<Arc<Self>> {
        let conn = store::open(path, now_s())?;
        Ok(Arc::new(Self {
            conn: Arc::new(Mutex::new(conn)),
            live: Mutex::new(Live::new(day_of(now_s()))),
        }))
    }

    fn live(&self) -> std::sync::MutexGuard<'_, Live> {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn conn(&self) -> &SharedConn {
        &self.conn
    }

    /// A handshake was accepted.
    pub fn session_started(
        &self,
        id: Uuid,
        account: Uuid,
        client: &'static str,
        build: String,
        protocol: u16,
    ) {
        let mut l = self.live();
        l.open.insert(id, account);
        l.sessions.push(SessionOp::Start {
            id,
            account,
            client,
            build,
            protocol,
            at: now_s(),
        });
    }

    /// The connection a session rode on closed.
    pub fn session_ended(&self, id: Uuid) {
        let mut l = self.live();
        if l.open.remove(&id).is_some() {
            l.sessions.push(SessionOp::End { id, at: now_s() });
        }
    }

    /// Different accounts signed in right now.
    #[must_use]
    pub fn online(&self) -> usize {
        self.live().open.values().collect::<HashSet<_>>().len()
    }

    /// A handshake was turned away. `caller` is the address and browser, for
    /// counting devices; never stored.
    pub fn refused(
        &self,
        reason: &'static str,
        protocol: u16,
        client: &'static str,
        build: String,
        caller: (&str, &str),
    ) {
        let mut l = self.live();
        let who = l.visitor(caller.0, caller.1, "refused");
        let t = l
            .refused
            .entry((reason, protocol, client, build))
            .or_default();
        t.seen.insert(who);
        t.views += 1;
        t.dirty = true;
    }

    pub fn event(&self, event: Event, community: Option<Uuid>, topic: Option<&str>) {
        self.events(event, 1, community, topic);
    }

    /// Count `event` once a day per `key`, however often it happens.
    pub fn event_once(&self, event: Event, key: &str) {
        let mut l = self.live();
        if l.once.insert((event.kind(), key.to_string())) {
            *l.events
                .entry((event.kind(), String::new(), String::new()))
                .or_default() += 1;
        }
    }

    pub fn events(&self, event: Event, n: i64, community: Option<Uuid>, topic: Option<&str>) {
        if n <= 0 {
            return;
        }
        let key = (
            event.kind(),
            community.map(|c| c.to_string()).unwrap_or_default(),
            topic.unwrap_or("").chars().take(64).collect(),
        );
        *self.live().events.entry(key).or_default() += n;
    }

    /// `account` did something in a community (a topic when `topic` is set):
    /// what "active members" and "people writing" count.
    pub fn active(&self, account: Uuid, community: Uuid, topic: Option<&str>) {
        let key = (
            account,
            community.to_string(),
            topic.unwrap_or("").chars().take(64).collect::<String>(),
        );
        let mut l = self.live();
        if l.activity_seen.insert(key.clone()) {
            l.activity_new.push(key);
        }
    }

    /// One page view (`POST /api/stats/hit`).
    pub fn web_hit(&self, hit: web::Hit, ip: &str, user_agent: &str) {
        let mut l = self.live();
        let who = l.visitor(ip, user_agent, hit.site);
        l.web(hit.site, "total", String::new(), who);
        l.web(hit.site, "path", hit.path, who);
        l.web(hit.site, "source", hit.source, who);
        l.web(hit.site, "device", hit.device.to_string(), who);
        l.web(hit.site, "lang", hit.lang, who);
    }

    /// A request from something that is not a person reading the page.
    pub fn web_bot(&self, site: &'static str) {
        let mut l = self.live();
        l.web(site, "bots", String::new(), 0);
    }

    /// Somebody who is not a member opened a community's public page or one of
    /// its posts.
    pub fn community_visit(&self, community: Uuid, ip: &str, user_agent: &str) {
        let mut l = self.live();
        let scope = format!("community:{community}");
        let who = l.visitor(ip, user_agent, &scope);
        l.web("community", "visit", community.to_string(), who);
    }

    /// Once a minute: who is signed in, and where each avatar stands.
    pub fn sample(&self, avatars: impl IntoIterator<Item = (String, f64, f64)>) {
        let now = now_s();
        let hour = now.div_euclid(3600);
        let mut l = self.live();
        let online = i64::try_from(l.open.values().collect::<HashSet<_>>().len()).unwrap_or(0);
        l.online = Some(match l.online {
            Some((h, sum, n, peak)) if h == hour => (h, sum + online, n + 1, peak.max(online)),
            _ => (hour, online, 1, online),
        });
        for (name, lat, lon) in avatars {
            let sq = l.squares.entry(square_of(lat, lon)).or_default();
            sq.minutes += 1;
            sq.people.insert(name);
        }
    }

    /// Write what was counted since the last call, and close the day when the
    /// date has moved on.
    pub async fn flush(&self) -> anyhow::Result<()> {
        let now = now_s();
        let batch = self.take(now);
        crate::db::call(&self.conn, move |c| store::write(c, &batch, now)).await?
    }

    fn take(&self, now: i64) -> store::Batch {
        let mut l = self.live();
        let day = l.day;
        let mut b = store::Batch {
            day,
            closes: None,
            ..Default::default()
        };
        b.sessions = std::mem::take(&mut l.sessions);
        for ((site, dim, value), t) in &mut l.web {
            if t.dirty {
                b.web.push(store::WebRow {
                    site,
                    dim,
                    value: value.clone(),
                    visitors: i64::try_from(t.seen.len()).unwrap_or(0),
                    views: t.views,
                });
                t.views = 0;
                t.dirty = false;
            }
        }
        for ((reason, protocol, client, build), t) in &mut l.refused {
            if t.dirty {
                b.refused.push(store::RefusedRow {
                    reason,
                    protocol: *protocol,
                    client,
                    build: build.clone(),
                    attempts: t.views,
                    devices: i64::try_from(t.seen.len()).unwrap_or(0),
                });
                t.views = 0;
                t.dirty = false;
            }
        }
        b.events = l.events.drain().collect();
        b.activity = std::mem::take(&mut l.activity_new);
        for (&(sx, sy), sq) in &mut l.squares {
            if sq.minutes > 0 {
                b.squares.push(store::SquareRow {
                    sx,
                    sy,
                    minutes: sq.minutes,
                    people: i64::try_from(sq.people.len()).unwrap_or(0),
                });
                sq.minutes = 0;
            }
        }
        // The hour's row is written every minute with its running totals; the
        // stored row is replaced, so only the current hour is held here.
        b.online = l.online;
        if day_of(now) != day {
            b.closes = Some(day);
            let open = std::mem::take(&mut l.open);
            *l = Live::new(day_of(now));
            l.open = open;
        }
        b
    }
}

/// Who is knocking: what a connection's carrier knew before the handshake, for
/// counting the session it becomes or the refusal it gets. The address and
/// browser only ever reach the daily hash.
pub struct Arrival {
    pub stats: Arc<Stats>,
    pub client: &'static str,
    pub build: String,
    pub ip: String,
    pub user_agent: String,
}

impl Arrival {
    pub fn refused(&self, reason: &'static str, protocol: u16) {
        self.stats.refused(
            reason,
            protocol,
            self.client,
            self.build.clone(),
            (&self.ip, &self.user_agent),
        );
    }
}

/// Sample and flush once a minute, for the life of the server.
pub fn spawn(
    stats: Arc<Stats>,
    world: Arc<tokio::sync::RwLock<crate::state::SimWorld>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut every = tokio::time::interval(std::time::Duration::from_secs(60));
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        every.tick().await;
        loop {
            every.tick().await;
            let avatars = world.read().await.stats_positions();
            stats.sample(avatars);
            if let Err(e) = stats.flush().await {
                tracing::warn!("stats flush: {e:#}");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squares_are_about_250_m_in_groningen() {
        // Grote Markt and a point 200 m east share a square or are neighbours;
        // 600 m east is never the same one.
        let a = square_of(53.2185, 6.5674);
        let b = square_of(53.2185, 6.5674 + 600.0 / 66_700.0);
        assert!(b.0 - a.0 >= 2);
        assert_eq!(a.1, b.1);
    }

    #[test]
    fn tags_are_closed_lists() {
        assert_eq!(client_tag(Some("android")), "android");
        assert_eq!(client_tag(Some("<script>")), "unknown");
        assert_eq!(client_tag(None), "unknown");
        assert_eq!(build_tag(Some("410d46f4abcdef0123")), "410d46f4abcd");
        assert_eq!(build_tag(Some("a'; drop")), "adrop");
    }

    #[tokio::test]
    async fn a_visitor_is_counted_once_a_day_and_views_add_up() {
        let stats = Stats::open(":memory:").unwrap();
        let hit = || web::Hit {
            site: "web",
            path: "/".into(),
            source: "direct".into(),
            device: "phone",
            lang: "nl".into(),
        };
        stats.web_hit(hit(), "192.0.2.1", "Firefox");
        stats.web_hit(hit(), "192.0.2.1", "Firefox");
        stats.web_hit(hit(), "192.0.2.2", "Firefox");
        stats.flush().await.unwrap();
        stats.web_hit(hit(), "192.0.2.1", "Firefox");
        stats.flush().await.unwrap();
        let (visitors, views): (i64, i64) = stats
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT visitors, views FROM web_daily WHERE site='web' AND dim='total'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((visitors, views), (2, 4));
    }

    #[tokio::test]
    async fn sessions_are_written_and_closed() {
        let stats = Stats::open(":memory:").unwrap();
        let (s, a) = (Uuid::new_v4(), Uuid::new_v4());
        stats.session_started(s, a, "android", "abc123".into(), 40);
        assert_eq!(stats.online(), 1);
        stats.flush().await.unwrap();
        stats.session_ended(s);
        assert_eq!(stats.online(), 0);
        stats.flush().await.unwrap();
        let ended: Option<i64> = stats
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT ended FROM sessions WHERE id=?1",
                [s.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(ended.is_some());
    }

    #[test]
    fn a_new_value_past_the_cap_is_other() {
        let mut l = Live::new(0);
        for i in 0..MAX_VALUES + 5 {
            l.web("web", "path", format!("/p{i}"), 1);
        }
        assert!(l.web.contains_key(&("web", "path", "(other)".to_string())));
        assert_eq!(l.values[&("web", "path")], MAX_VALUES);
    }
}
