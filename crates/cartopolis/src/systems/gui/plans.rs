//! Layers ▸ Places ▸ **Plans**: what the municipality has published about a
//! place, at the place, and the council business that names it
//! (`docs/notes/plans.md`; the design is `docs/mockups/plans-and-decisions/`,
//! v2 for the layer and v1 for the cards).
//!
//! The feed is atlas's `/plans/groningen.json` (`atlas::plans` builds it,
//! `cartopolis_core::plans` is its shape): a year of the Gemeenteblad's
//! notices, each with the point or area it carries, and the council items
//! placed by the street names in their titles. Cached for [`FEED_TTL`], read
//! again when the layer is on and the copy is stale, and kept on a failed read.
//!
//! **Drawn over the map, not meshed**, for the reason saved places are
//! (`systems::place_pins`): a notice is a remark *about* a spot, it has to be
//! findable from the street and from a few kilometres up, and so it is a dot
//! of fixed screen size over the map. An orange dot's response period still
//! runs; a grey one is history. A council item is a blue square on the place
//! its title names. A work in the city's maintenance programme is a yellow
//! square at its pin over the outline of where it happens — solid on the
//! street, dashed under it, rings on its trees (the design's v3).
//!
//! **What is drawn** is chosen in the tile's ⋯: the kinds of notice, whether
//! the council and the works are shown, and how far back the notices go
//! ([`Period`]; works are per year). With the layer off
//! nothing is fetched or drawn, and the map ring's *Plans here* still answers
//! for one spot — it loads the feed for that.
//!
//! [`drive_plans`] fetches and takes the ring's request, [`place_plans`]
//! places the dots, squares and outlines (culled against the map area, as the
//! pins are) and does what a tap on one asked, and [`drive_plans_card`] and
//! [`drive_plans_news`] decide when the card a tap opens and the news chip
//! stand aside; the library draws the marks, the chip, the card and the
//! news (`lib_plans`).

use cartopolis_core::msg;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use big_space::prelude::GridCell;
use cartopolis_core::plans::{
    civil_date, days_between, metres, near_followed, notice_url, work_near_followed, Notice,
    NoticeKind, PlansFeed, Work, WorkKind, NEAR_M as HERE_M,
};
use web_time::{Instant, SystemTime, UNIX_EPOCH};

use crate::components::FreeCamera;
use crate::grid::anchored_position_f64;
use crate::platform::{http, storage};
use crate::resources::MapAnchor;
use crate::systems::globe::GlobeState;
use crate::systems::layer_state::DataLayers;
use crate::systems::place_pins::{clear_of_chrome, map_area, place_delta};
use crate::systems::ui_input::{UiInput, UiMetrics};
use crate::systems::{icons, theme};

/// Atlas's feed. The browser build asks its own origin, which is the same
/// server.
const FEED_URL: &str = "https://cartopolis.org/plans/groningen.json";
const FEED_URL_WEB: &str = "/plans/groningen.json";
/// Where the feed is kept between runs (`platform::storage`'s cache).
const FEED_KEY: &str = "plans/groningen.json";
/// How old the cached feed may be before it is read again. Atlas rebuilds it
/// every three hours; notices are published on working days.
const FEED_TTL: Duration = Duration::from_secs(3 * 3600);
/// How long a failed read waits before it is tried again.
const RETRY_AFTER: Duration = Duration::from_secs(300);

/// Where the feed has anything to say: the municipality of Groningen, a little
/// generously, `[south, west, north, east]`. The ring offers *Plans here* only
/// inside it, before the feed's own bbox is known.
const COVERED: [f64; 4] = [53.10, 6.40, 53.33, 6.90];

/// Above this camera altitude the dots are not drawn: a city's year of notices
/// is one orange smudge from higher, and every dot is a node of its own.
const MAX_ALTITUDE_M: f64 = 12_000.0;
/// At most this many dots a frame, the newest first.
const MAX_DOTS: usize = 400;
/// At most this many pieces of outline a frame (`PlanStroke`): each is a
/// node, and a dashed sewer across the view is hundreds of dashes.
const MAX_STROKES: usize = 3_000;
/// A dot's radius, points; an open one's.
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub(crate) const DOT_R: f32 = 5.5;
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub(crate) const OPEN_R: f32 = 6.5;
/// The council square's half-size, points.
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub(crate) const COUNCIL_R: f32 = 9.0;
/// The outlines' width, points; a notice area's dash and gap, and a work's
/// under the street.
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub(crate) const STROKE_W: f32 = 2.0;
const AREA_DASH: (f32, f32) = (6.0, 4.0);
const WORK_DASH: (f32, f32) = (7.0, 4.0);
/// A tree's ring's radius, points.
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub(crate) const TREE_R: f32 = 4.0;
/// How near counts as "here" for a card's "Also here", metres. The ring's
/// *Plans here* and a followed place use the shared `HERE_M`.
const ALSO_HERE_M: f64 = 40.0;
/// The works' colour: not something to respond to, so not the notices'
/// orange, but something that will happen in the street (the design's v3).
pub const WORK: theme::Rgba8 = theme::Rgba8::from_rgb(0xF5, 0xC5, 0x18);
/// A work's square marker's half-size, points.
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub(crate) const WORK_R: f32 = 9.0;
/// At most this many works a frame.
const MAX_WORKS: usize = 200;

pub use crate::systems::layer_state::Period;

impl Period {
    pub const ALL: [Self; 3] = [Self::Open, Self::ThreeMonths, Self::Year];

    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::ThreeMonths => "3m",
            Self::Year => "1y",
        }
    }

    #[must_use]
    pub fn from_key(key: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|p| p.key() == key)
            .unwrap_or_default()
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Open only",
            Self::ThreeMonths => "3 months",
            Self::Year => "Year",
        }
    }

    /// The earliest publication date shown, `YYYY-MM-DD`.
    fn since(self, now_unix: u64) -> String {
        let days = match self {
            Self::Open | Self::Year => 366,
            Self::ThreeMonths => 92,
        };
        civil_date(now_unix.saturating_sub(days * 86_400))
    }
}

/// The filters' kinds, in the order the ⋯ lists them, with their labels and
/// whether a fresh install shows them. Events and traffic orders are off by
/// default: they are many, short-lived, and drawn as routes across the whole
/// centre (the design's v1 note).
pub const KINDS: [(NoticeKind, &str, bool); crate::systems::layer_state::PLAN_KINDS] = [
    (NoticeKind::Application, "Applications", true),
    (NoticeKind::Decision, "Decisions", true),
    (NoticeKind::Notification, "Notifications", true),
    (NoticeKind::Event, "Events", false),
    (NoticeKind::Traffic, "Traffic", false),
    (NoticeKind::Other, "Other", true),
];

/// The kinds a fresh install shows.
#[must_use]
pub fn default_kinds() -> [bool; KINDS.len()] {
    KINDS.map(|(_, _, on)| on)
}

/// The kinds as saved in `user.json`, by key.
#[must_use]
pub fn kinds_to_prefs(kinds: &[bool; KINDS.len()]) -> std::collections::HashMap<String, bool> {
    KINDS
        .iter()
        .zip(kinds)
        .map(|((k, _, _), on)| (k.key().to_string(), *on))
        .collect()
}

/// The kinds from `user.json`; a kind missing there takes its default.
#[must_use]
pub fn kinds_from_prefs(p: &std::collections::HashMap<String, bool>) -> [bool; KINDS.len()] {
    KINDS.map(|(k, _, on)| p.get(k.key()).copied().unwrap_or(on))
}

/// Whether the ring's *Plans here* has anything to answer at `(lat, lng)`.
#[must_use]
pub fn covers(lat: f64, lng: f64) -> bool {
    let [s, w, n, e] = COVERED;
    lat >= s && lat <= n && lng >= w && lng <= e
}

/// Places followed for new plans (`UserData::plans_follow`, protocol 50).
///
/// A notice published within `HERE_M` of one is news until it has been shown:
/// the chip over the map says so, and the server wakes a closed phone for it
/// (`simulator::plans_watch`). The server holds the places too; what this
/// device has shown is its own.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct PlansFollow {
    /// `[lat, lng]`, in the order they were followed.
    #[serde(default)]
    pub places: Vec<[f64; 2]>,
    /// Notices and works near a followed place that were there when it was
    /// followed or have been shown since, by id.
    #[serde(default)]
    pub seen: Vec<String>,
}

/// Every notice and work near `place`, by id.
fn ids_near(place: [f64; 2], feed: &PlansFeed) -> impl Iterator<Item = String> + '_ {
    let notices = feed
        .notices
        .iter()
        .filter(move |n| near_followed(place, n))
        .map(|n| n.id.clone());
    let works = feed
        .works
        .iter()
        .filter(move |w| work_near_followed(place, w))
        .map(|w| w.id.clone());
    notices.chain(works)
}

/// Most places one device follows; the server keeps as many.
pub const MAX_FOLLOWS: usize = 20;

/// A spot this near a followed place is that place: the ring is opened by a
/// finger, never twice on the same metre.
const SAME_PLACE_M: f64 = 25.0;

impl PlansFollow {
    /// The followed place at `at`, if there is one.
    #[must_use]
    pub fn at(&self, at: (f64, f64)) -> Option<usize> {
        self.places
            .iter()
            .position(|p| metres(at, (p[0], p[1])) <= SAME_PLACE_M)
    }

    /// Follow `at`, with every notice already published near it counted as
    /// seen; or stop following it.
    pub fn toggle(&mut self, at: (f64, f64), feed: Option<&PlansFeed>) {
        if let Some(i) = self.at(at) {
            self.places.remove(i);
            return;
        }
        if self.places.len() >= MAX_FOLLOWS {
            self.places.remove(0);
        }
        let place = [at.0, at.1];
        self.places.push(place);
        if let Some(feed) = feed {
            self.seen.extend(ids_near(place, feed));
        }
    }

    /// Notices and works near a followed place not shown yet — the notices
    /// newest first, then the works — as the card each opens and the place it
    /// is near.
    #[must_use]
    pub fn news(&self, feed: &PlansFeed) -> Vec<(Card, [f64; 2])> {
        if self.places.is_empty() {
            return Vec::new();
        }
        let seen: std::collections::HashSet<&str> = self.seen.iter().map(String::as_str).collect();
        let notices = feed
            .notices
            .iter()
            .enumerate()
            .filter(|(_, n)| !seen.contains(n.id.as_str()))
            .filter_map(|(i, n)| {
                self.places
                    .iter()
                    .find(|p| near_followed(**p, n))
                    .map(|p| (Card::Notice(i), *p))
            });
        let works = feed
            .works
            .iter()
            .enumerate()
            .filter(|(_, w)| !seen.contains(w.id.as_str()))
            .filter_map(|(i, w)| {
                self.places
                    .iter()
                    .find(|p| work_near_followed(**p, w))
                    .map(|p| (Card::Work(i), *p))
            });
        notices.chain(works).collect()
    }

    /// Count everything near `place` as shown, and forget shown notices and
    /// works the feed no longer holds, so the list does not grow for ever.
    pub fn mark_seen(&mut self, place: [f64; 2], feed: &PlansFeed) {
        self.seen.extend(ids_near(place, feed));
        let ids: std::collections::HashSet<&str> = feed
            .notices
            .iter()
            .map(|n| n.id.as_str())
            .chain(feed.works.iter().map(|w| w.id.as_str()))
            .collect();
        self.seen.retain(|id| ids.contains(id.as_str()));
        self.seen.sort();
        self.seen.dedup();
    }
}

/// What the card shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Card {
    /// A notice, by its index in the feed.
    Notice(usize),
    /// A council item, by its index in the feed.
    Council(usize),
    /// A work in the maintenance programme, by its index in the feed.
    Work(usize),
    /// Everything published around a spot.
    Here { lat: f64, lng: f64 },
}

/// The layer's state.
#[derive(Resource, Default)]
pub struct Plans {
    feed: Option<Arc<PlansFeed>>,
    loading: bool,
    loaded_at: Option<Instant>,
    failed_at: Option<Instant>,
    inbox: Arc<Mutex<Option<Option<PlansFeed>>>>,
    /// The card a tap opened.
    pub card: Option<Card>,
    /// Open the card of the notice nearest this spot that names council
    /// business, once the feed is in: `--ui plansnotice`, the one card a
    /// scripted run cannot reach by tapping.
    pub open_nearest: Option<(f64, f64)>,
    /// Dots drawn last frame, how many of them still open, and works: the
    /// chip.
    shown: (usize, usize, usize),
    /// An agent asked about notices (`mcp::world`): read the feed as if the
    /// layer were on, without drawing it on the person's map.
    pub agent_wants: bool,
    /// Notices and works near a followed place not shown yet
    /// ([`PlansFollow::news`]).
    news: Vec<(Card, [f64; 2])>,
    /// What [`place_plans`], [`drive_plans_card`] and [`drive_plans_news`]
    /// work out for the library's marks, chip, card and news (`lib_plans`).
    pub lib_view: PlansView,
    /// The key of the mark the library's tap landed on ([`PlanMark::key`]),
    /// opened next frame by [`place_plans`].
    pub lib_tapped: Option<String>,
}

/// What the plans systems work out for the library (`lib_plans`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlansView {
    /// The layer's chip, when it is up: dots drawn, how many still open,
    /// works, and whether the view is too high to draw them.
    pub chip: Option<(usize, usize, usize, bool)>,
    /// The place card's reasons to stand aside hold: the card steps aside.
    pub card_hidden: bool,
    /// The news chip stands aside (a card, a panel, the menu, driving).
    pub news_hidden: bool,
    /// The dots and squares over the map this frame, in the order they
    /// stack (the last on top).
    pub marks: Vec<PlanMark>,
    /// The outlines under every mark: a notice's area, a work's lines and
    /// areas (dashed under the street), rings on its trees.
    pub strokes: Vec<PlanStroke>,
}

/// One tappable mark where it stands this frame, in UI points.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct PlanMark {
    /// Stable across frames: what the library keys the mark's node by, and
    /// what a tap on it comes back as.
    pub key: String,
    pub at: Vec2,
    /// What a pointer resting on it is told.
    pub title: String,
    pub look: MarkLook,
}

/// What a mark looks like.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub enum MarkLook {
    /// A notice's dot: orange while its response period runs, grey after.
    Notice { open: bool },
    /// A council item's blue square: a tick once decided, a dot before.
    Council { decided: bool },
    /// A work's yellow square, with its kind's glyph.
    Work { glyph: &'static str },
}

/// One piece of outline this frame, in UI points, already clipped to the
/// map.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct PlanStroke {
    /// Stable across frames while the shape stays in view.
    pub key: String,
    pub look: StrokeLook,
}

/// A straight piece `STROKE_W` wide from `a` to `b`, or a tree's ring.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub enum StrokeLook {
    Line { a: Vec2, b: Vec2, ink: StrokeInk },
    Ring { at: Vec2 },
}

/// Which colour an outline is: an open notice's, a closed one's, a work's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrokeInk {
    Open,
    Closed,
    Work,
}

impl StrokeInk {
    #[must_use]
    #[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
    pub fn colour(self) -> theme::Rgba8 {
        match self {
            StrokeInk::Open => theme::BRAND,
            StrokeInk::Closed => theme::DARK.accent,
            StrokeInk::Work => WORK,
        }
    }
}

impl Plans {
    /// The feed as last read, if it has been.
    #[cfg_attr(not(mcp), allow(dead_code))]
    pub fn feed(&self) -> Option<Arc<PlansFeed>> {
        self.feed.clone()
    }

    /// The last read failed and nothing was kept: an empty answer is "could
    /// not read it", not "nothing published".
    #[cfg_attr(not(mcp), allow(dead_code))]
    pub fn failed(&self) -> bool {
        self.feed.is_none() && self.failed_at.is_some()
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The URL the feed is read from: atlas, or `CARTO_PLANS_URL`.
fn feed_url() -> String {
    crate::env_value("CARTO_PLANS_URL").unwrap_or_else(|| {
        if cfg!(target_arch = "wasm32") {
            FEED_URL_WEB.to_string()
        } else {
            FEED_URL.to_string()
        }
    })
}

/// The feed: the cache while it is fresh, else a read (cached on success),
/// else whatever stale copy there is.
async fn load_feed() -> Option<PlansFeed> {
    let parse = |text: &str| serde_json::from_str::<PlansFeed>(text).ok();
    let fresh = storage::age_secs(storage::Namespace::Cache, FEED_KEY)
        .await
        .is_some_and(|age| age < FEED_TTL.as_secs_f64());
    if fresh {
        if let Some(feed) = storage::get_string(storage::Namespace::Cache, FEED_KEY)
            .await
            .and_then(|t| parse(&t))
        {
            return Some(feed);
        }
    }
    let url = feed_url();
    match http::fetch_string(http::Request::get(&url).timeout(Duration::from_secs(60))).await {
        Ok(text) => match parse(&text) {
            Some(feed) => {
                storage::put(storage::Namespace::Cache, FEED_KEY, text.as_bytes()).await;
                Some(feed)
            }
            None => {
                tracing::warn!(%url, "plans feed: answer is not a feed");
                storage::get_string(storage::Namespace::Cache, FEED_KEY)
                    .await
                    .and_then(|t| parse(&t))
            }
        },
        Err(e) => {
            tracing::warn!(%url, error = %e, "plans feed read failed");
            storage::get_string(storage::Namespace::Cache, FEED_KEY)
                .await
                .and_then(|t| parse(&t))
        }
    }
}

/// Read the feed when it is wanted and stale, fold in what arrived, and take
/// the ring's *Plans here*.
pub fn drive_plans(
    mut plans: ResMut<Plans>,
    dl: Res<DataLayers>,
    mut context_menu: ResMut<crate::resources::ContextMenuState>,
    store: Res<crate::systems::user_store::UserStore>,
) {
    let plans = &mut *plans;
    if let Some((lat, lng)) = context_menu.plans_here.take() {
        plans.card = Some(Card::Here { lat, lng });
    }
    let arrived = plans.inbox.lock().ok().and_then(|mut g| g.take());
    let follow = &store.data.plans_follow;
    if arrived.is_some() || store.is_changed() {
        plans.news = arrived
            .as_ref()
            .and_then(Option::as_ref)
            .or(plans.feed.as_deref())
            .map(|f| follow.news(f))
            .unwrap_or_default();
    }
    if let Some(result) = arrived {
        plans.loading = false;
        match result {
            Some(feed) => {
                tracing::info!(
                    notices = feed.notices.len(),
                    council = feed.council.len(),
                    works = feed.works.len(),
                    "plans feed loaded"
                );
                plans.feed = Some(Arc::new(feed));
                plans.loaded_at = Some(Instant::now());
                plans.failed_at = None;
            }
            None => plans.failed_at = Some(Instant::now()),
        }
        // A card pointing into a feed that was replaced points at something else.
        if matches!(
            plans.card,
            Some(Card::Notice(_) | Card::Council(_) | Card::Work(_))
        ) {
            plans.card = None;
        }
    }
    if let (Some(at), Some(feed)) = (plans.open_nearest, plans.feed.clone()) {
        plans.open_nearest = None;
        let nearest = feed
            .notices
            .iter()
            .enumerate()
            .filter(|(_, n)| !n.council.is_empty())
            .min_by(|(_, a), (_, b)| {
                metres(at, (a.lat, a.lng)).total_cmp(&metres(at, (b.lat, b.lng)))
            });
        if let Some((i, _)) = nearest {
            plans.card = Some(Card::Notice(i));
        }
    }
    // A followed place keeps the feed read, or its news would wait for the
    // layer to be turned on.
    let wanted = dl.plans_enabled
        || plans.card.is_some()
        || plans.open_nearest.is_some()
        || plans.agent_wants
        || !follow.places.is_empty();
    let stale = plans.loaded_at.is_none_or(|t| t.elapsed() >= FEED_TTL);
    let backing_off = plans.failed_at.is_some_and(|t| t.elapsed() < RETRY_AFTER);
    if wanted && stale && !plans.loading && !backing_off {
        plans.loading = true;
        let inbox = plans.inbox.clone();
        IoTaskPool::get()
            .spawn(async move {
                let feed = load_feed().await;
                if let Ok(mut g) = inbox.lock() {
                    *g = Some(feed);
                }
            })
            .detach();
    }
}

/// Tell the server the places this device follows for new plans: once a
/// connection when there are any, and whenever they change. A device that
/// follows nothing says nothing on connecting, so it does not clear what the
/// account follows on another device.
pub fn send_plan_follows(
    online: Option<Res<crate::resources::OnlineSession>>,
    store: Res<crate::systems::user_store::UserStore>,
    mut sent: Local<Option<Vec<[f64; 2]>>>,
) {
    let Some(online) = online else {
        *sent = None;
        return;
    };
    if online.is_added() {
        *sent = None;
    }
    let places = &store.data.plans_follow.places;
    let due = match &*sent {
        None => !places.is_empty(),
        Some(was) => was != places,
    };
    if due {
        online.send(
            "followed places",
            cartopolis_core::NetMessage::FollowPlans(msg::FollowPlans {
                places: places.clone(),
            }),
        );
        tracing::info!(
            places = places.len(),
            "followed places told to the simulator"
        );
    }
    if due || sent.is_none() {
        *sent = Some(places.clone());
    }
}

/// Whether a notice passes the layer's filters on `today`.
fn shown(n: &Notice, dl: &DataLayers, today: &str, since: &str) -> bool {
    let kind_on = KINDS
        .iter()
        .position(|(k, _, _)| *k == n.kind)
        .is_some_and(|i| dl.plans_kinds[i]);
    if !kind_on {
        return false;
    }
    match dl.plans_period {
        Period::Open => n.open_on(today),
        _ => n.date.as_str() >= since,
    }
}

/// The dots, areas and squares over the map, placed for the library
/// (`lib_plans`), the counts for the chip, and the card a tap on a mark
/// opens. Before the pins, which stand over them.
#[allow(clippy::too_many_arguments)]
pub fn place_plans(
    mut plans: ResMut<Plans>,
    dl: Res<DataLayers>,
    globe: Res<GlobeState>,
    camera: Query<(&Camera, &GridCell, &Transform), With<FreeCamera>>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    map_anchor: Res<MapAnchor>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    lens: Res<crate::systems::scenario::LensSlot>,
    route: Res<crate::systems::navigation::RouteState>,
    driver: Res<crate::systems::driver::DriverState>,
    right_panel: Res<crate::resources::RightPanel>,
    chrome: Res<crate::systems::dock::MapChrome>,
    input: Res<UiInput>,
    metrics: Res<UiMetrics>,
) {
    plans.shown = (0, 0, 0);
    if plans.lib_view.chip.is_some() {
        plans.lib_view.chip = None;
    }
    // Cleared first, so each early return below leaves the map bare.
    if !plans.lib_view.marks.is_empty() {
        plans.lib_view.marks.clear();
    }
    if !plans.lib_view.strokes.is_empty() {
        plans.lib_view.strokes.clear();
    }
    let tapped_key = plans.lib_tapped.take();
    if !dl.plans_enabled || driver.active {
        return;
    }
    let Some(feed) = plans.feed.clone() else {
        return;
    };
    let Some(anchor) = map_anchor.geo else {
        return;
    };
    let Ok((cam, cam_cell, cam_tf)) = camera.single() else {
        return;
    };
    let in_space = globe.in_space(cam_cell, cam_tf);
    let high = in_space || globe.altitude_m(cam_cell, cam_tf) > MAX_ALTITUDE_M;

    let map_rect = map_area(&input, &metrics, &keyboard, &route, &lens);
    let cam_world = anchored_position_f64(cam_cell, cam_tf.translation);
    // This frame's camera basis, not last frame's (`place_pins` says why).
    let cam_view = GlobalTransform::from(Transform::from_rotation(cam_tf.rotation));
    let project = |lat: f64, lng: f64| -> Option<Vec2> {
        let delta = place_delta(&globe, anchor, cam_world, in_space, lat, lng, &terrain)?;
        let screen = cam.world_to_viewport(&cam_view, delta.as_vec3()).ok()?;
        Some(metrics.window_to_ui(screen))
    };

    let now = now_unix();
    let today = civil_date(now);
    let since = dl.plans_period.since(now);
    // Every mark in the order it stacks, and the card a tap on it opens.
    let mut marks: Vec<(PlanMark, Card)> = Vec::new();
    let mut strokes = Strokes::new(map_rect);
    let (mut drawn, mut open, mut works) = (0usize, 0usize, 0usize);
    if !high {
        // The works first, under the notices' dots: this year's and later.
        if dl.plans_works {
            for (i, w) in feed.works.iter().enumerate() {
                if works >= MAX_WORKS {
                    break;
                }
                if !current(w, &today) {
                    continue;
                }
                let Some(pin) = project(w.lat, w.lng) else {
                    continue;
                };
                if !map_rect.contains(pin) {
                    continue;
                }
                works += 1;
                work_shape(&mut strokes, i, w, &project);
                if !clear_of_chrome(&chrome.holes, pin) {
                    continue;
                }
                marks.push((
                    PlanMark {
                        key: format!("work:{i}"),
                        at: pin,
                        title: format!("Works {}: {}", w.year, w.measure),
                        look: MarkLook::Work {
                            glyph: work_icon(w.kind),
                        },
                    },
                    Card::Work(i),
                ));
            }
        }
        // Newest first, as the feed is: when the cap bites, the old go.
        for (i, n) in feed.notices.iter().enumerate() {
            if drawn >= MAX_DOTS {
                break;
            }
            if !shown(n, &dl, &today, &since) {
                continue;
            }
            let Some(tip) = project(n.lat, n.lng) else {
                continue;
            };
            if !map_rect.contains(tip) || !clear_of_chrome(&chrome.holes, tip) {
                continue;
            }
            let is_open = n.open_on(&today);
            drawn += 1;
            open += usize::from(is_open);
            // Its area's outline, under the dot, when it was published as one.
            if n.area.len() >= 3 {
                let ring: Option<Vec<Vec2>> = n.area.iter().map(|p| project(p[0], p[1])).collect();
                if let Some(mut ring) = ring {
                    ring.push(ring[0]);
                    let ink = if is_open {
                        StrokeInk::Open
                    } else {
                        StrokeInk::Closed
                    };
                    strokes.dashed(&format!("notice:{i}"), &ring, AREA_DASH, ink);
                }
            }
            marks.push((
                PlanMark {
                    key: format!("notice:{i}"),
                    at: tip,
                    title: n.title.clone(),
                    look: MarkLook::Notice { open: is_open },
                },
                Card::Notice(i),
            ));
        }
        if dl.plans_council {
            for (i, c) in feed.council.iter().enumerate() {
                if c.last_date() < since.as_str() {
                    continue;
                }
                for (j, p) in c.places.iter().enumerate() {
                    let Some(at) = project(p.lat, p.lng) else {
                        continue;
                    };
                    if !map_rect.contains(at) || !clear_of_chrome(&chrome.holes, at) {
                        continue;
                    }
                    marks.push((
                        PlanMark {
                            key: format!("council:{i}:{j}"),
                            at,
                            title: c.title.clone(),
                            look: MarkLook::Council { decided: c.decided },
                        },
                        Card::Council(i),
                    ));
                }
            }
        }
    }
    plans.shown = (drawn, open, works);
    // A tap the library took last frame, on a mark that is still here.
    if let Some(card) = tapped_key
        .and_then(|key| marks.iter().find(|(m, _)| m.key == key))
        .map(|(_, card)| *card)
    {
        plans.card = Some(card);
    }
    plans.lib_view.marks = marks.into_iter().map(|(m, _)| m).collect();
    plans.lib_view.strokes = strokes.out;
    if right_panel.is(crate::resources::RightPanel::None) {
        // The library's chip (`lib_plans`) is drawn from the counts.
        plans.lib_view.chip = Some((drawn, open, works, high));
    }
}

/// The outline pieces of one frame, cut to the map and capped at
/// [`MAX_STROKES`]: what the library draws as thin turned boxes.
struct Strokes {
    map: Rect,
    out: Vec<PlanStroke>,
}

impl Strokes {
    fn new(map: Rect) -> Self {
        Strokes {
            map,
            out: Vec::new(),
        }
    }

    fn push(&mut self, key: String, look: StrokeLook) {
        if self.out.len() < MAX_STROKES {
            self.out.push(PlanStroke { key, look });
        }
    }

    /// A solid path: a piece per segment, each cut to the map.
    fn solid(&mut self, key: &str, pts: &[Vec2], ink: StrokeInk) {
        for (k, w) in pts.windows(2).enumerate() {
            if let Some((t0, t1)) = clip_params(w[0], w[1], self.map) {
                let d = w[1] - w[0];
                let (a, b) = (w[0] + d * t0, w[0] + d * t1);
                self.push(format!("{key}:{k}"), StrokeLook::Line { a, b, ink });
            }
        }
    }

    /// A dashed path (`dash` on, `gap` off), the pattern running on across
    /// its corners.
    fn dashed(&mut self, key: &str, pts: &[Vec2], (dash, gap): (f32, f32), ink: StrokeInk) {
        for (seg, n, a, b) in dashes(pts, dash, gap, self.map) {
            self.push(format!("{key}:{seg}:{n}"), StrokeLook::Line { a, b, ink });
        }
    }

    /// A tree's ring, when its centre is on the map.
    fn ring(&mut self, key: String, at: Vec2) {
        if self.map.contains(at) {
            self.push(key, StrokeLook::Ring { at });
        }
    }
}

/// Where the segment `a`–`b` runs inside `r`, as the share of the way from
/// `a` it enters and leaves (Liang–Barsky); `None` when it misses.
fn clip_params(a: Vec2, b: Vec2, r: Rect) -> Option<(f32, f32)> {
    if !a.is_finite() || !b.is_finite() {
        return None;
    }
    let d = b - a;
    let (mut t0, mut t1) = (0.0_f32, 1.0_f32);
    for (p, q) in [
        (-d.x, a.x - r.min.x),
        (d.x, r.max.x - a.x),
        (-d.y, a.y - r.min.y),
        (d.y, r.max.y - a.y),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                if t > t1 {
                    return None;
                }
                t0 = t0.max(t);
            } else {
                if t < t0 {
                    return None;
                }
                t1 = t1.min(t);
            }
        }
    }
    (t1 > t0).then_some((t0, t1))
}

/// The dashes of a path, `dash` on and `gap` off, the pattern running on
/// across its corners: only those inside `clip`, cut to it, each as its
/// segment, its number along the whole path, and its two ends. A path that
/// runs far off the screen costs only the part on it.
fn dashes(pts: &[Vec2], dash: f32, gap: f32, clip: Rect) -> Vec<(usize, usize, Vec2, Vec2)> {
    let period = dash + gap;
    let mut out = Vec::new();
    if period <= 0.0 {
        return out;
    }
    // How far along the path the segment starts.
    let mut walked = 0.0_f32;
    for (seg, w) in pts.windows(2).enumerate() {
        let (a, b) = (w[0], w[1]);
        let len = a.distance(b);
        if !len.is_finite() || len <= 0.0 {
            continue;
        }
        if let Some((t0, t1)) = clip_params(a, b, clip) {
            let (s0, s1) = (walked + t0 * len, walked + t1 * len);
            let at = |s: f32| a + (b - a) * ((s - walked) / len);
            let first = (s0 / period).floor().max(0.0) as usize;
            let last = (s1 / period).floor().max(0.0) as usize;
            for n in first..=last {
                let on = n as f32 * period;
                let (u0, u1) = (on.max(s0), (on + dash).min(s1));
                if u1 > u0 {
                    out.push((seg, n, at(u0), at(u1)));
                }
            }
        }
        walked += len;
    }
    out
}

/// Whether a work is planned for this year or later, `today` being
/// `YYYY-MM-DD`: the programme is per year and leaves the map when its year
/// has passed.
fn current(w: &Work, today: &str) -> bool {
    today
        .get(..4)
        .and_then(|y| y.parse::<u16>().ok())
        .is_none_or(|year| w.year >= year)
}

/// A work's shape on the map, in the works' colour, styled by where it
/// happens: solid on the street, dashed under it, rings on its trees.
fn work_shape(
    strokes: &mut Strokes,
    i: usize,
    w: &Work,
    project: &impl Fn(f64, f64) -> Option<Vec2>,
) {
    let path = |pts: &[[f64; 2]], closed: bool| -> Option<Vec<Vec2>> {
        let mut out: Vec<Vec2> = pts
            .iter()
            .map(|p| project(p[0], p[1]))
            .collect::<Option<_>>()?;
        if closed && !out.is_empty() {
            out.push(out[0]);
        }
        Some(out)
    };
    let draw = |strokes: &mut Strokes, key: String, pts: Vec<Vec2>| {
        if w.kind.underground() {
            strokes.dashed(&key, &pts, WORK_DASH, StrokeInk::Work);
        } else {
            strokes.solid(&key, &pts, StrokeInk::Work);
        }
    };
    for (k, ring) in w.areas.iter().enumerate() {
        if let Some(pts) = path(ring, true) {
            draw(strokes, format!("work:{i}:area:{k}"), pts);
        }
    }
    for (k, line) in w.lines.iter().enumerate() {
        if let Some(pts) = path(line, false) {
            draw(strokes, format!("work:{i}:line:{k}"), pts);
        }
    }
    for (k, p) in w.points.iter().enumerate() {
        if let Some(at) = project(p[0], p[1]) {
            strokes.ring(format!("work:{i}:tree:{k}"), at);
        }
    }
}

/// The glyph a kind of work is marked with.
fn work_icon(kind: WorkKind) -> &'static str {
    match kind {
        WorkKind::Road => icons::ROAD,
        WorkKind::Sewer => icons::PLUMBING,
        WorkKind::Trees => icons::GRASS,
        WorkKind::Lighting | WorkKind::Structure => icons::BUILD,
    }
}

/// What a kind of work is called on a card.
fn work_label(kind: WorkKind) -> &'static str {
    match kind {
        WorkKind::Road => "road",
        WorkKind::Sewer => "sewer",
        WorkKind::Trees => "trees",
        WorkKind::Lighting => "street-light cables",
        WorkKind::Structure => "bridges and quays",
    }
}

/// One plain sentence for the programme's commonest measures, by a word in
/// its Dutch; `None` for one it does not know, which is then shown alone.
fn gloss(measure: &str) -> Option<&'static str> {
    let m = measure.to_lowercase();
    let known: [(&str, &str); 10] = [
        (
            "huisaansluiting",
            "The pipes from the houses to the sewer are replaced.",
        ),
        (
            "relin",
            "A new lining is pulled through the old pipe, so the street is mostly not dug open.",
        ),
        (
            "hwa",
            "A separate pipe for rainwater (HWA), so it no longer runs into the sewer.",
        ),
        (
            "wadi",
            "A wadi: a low green strip that holds rainwater until it soaks away.",
        ),
        (
            "wortelopdruk",
            "Paving lifted by tree roots is levelled, and the roots given room.",
        ),
        (
            "boomstructuur",
            "The trees along the street are replaced or added to.",
        ),
        (
            "herstraten",
            "The bricks or tiles are taken up and laid again on a level bed.",
        ),
        ("asfalt", "A new top layer of asphalt."),
        (
            "bijdrage project",
            "The roads programme's share in a larger project here.",
        ),
        (
            "kabel",
            "The cable that feeds the street lights is replaced.",
        ),
    ];
    known
        .iter()
        .find(|(word, _)| m.contains(word))
        .map(|(_, line)| *line)
}

/// The layer's chip's words (`lib_plans` draws it): how many are drawn (or
/// that the view is too high), how many still open, and the works planned.
#[must_use]
pub fn chip_words(
    counts: (usize, usize, usize),
    high: bool,
) -> (String, Option<String>, Option<String>) {
    let (drawn, open, works) = counts;
    let what = if high {
        "zoom in to see them".to_string()
    } else {
        format!("{drawn} here")
    };
    let open = (!high && open > 0).then(|| format!("{open} still open"));
    let works = (!high && works > 0).then(|| {
        let what = if works == 1 { "work" } else { "works" };
        format!("{works} {what} planned")
    });
    (what, open, works)
}

/// `28 Oct 2026` for `2026-10-28`.
fn pretty_date(date: &str) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mut it = date.split('-');
    match (
        it.next(),
        it.next().and_then(|m| m.parse::<usize>().ok()),
        it.next(),
    ) {
        (Some(y), Some(m @ 1..=12), Some(d)) => {
            format!("{} {} {y}", d.trim_start_matches('0'), MONTHS[m - 1])
        }
        _ => date.to_string(),
    }
}

/// What a kind is called on a card.
fn kind_label(kind: NoticeKind) -> &'static str {
    match kind {
        NoticeKind::Application => "Permit application",
        NoticeKind::Decision => "Decision",
        NoticeKind::Notification => "Notification",
        NoticeKind::Event => "Event licence",
        NoticeKind::Traffic => "Traffic order",
        NoticeKind::Other => "Notice",
    }
}

/// The line about a notice's response period, and whether it still runs.
fn deadline(n: &Notice, today: &str) -> Option<(String, bool)> {
    let until = n.respond_until.as_deref()?;
    let verb = match n.kind {
        NoticeKind::Decision => "Objections",
        _ => "Responses",
    };
    Some(match days_between(today, until) {
        Some(d) if d >= 0 => {
            let days = if d == 1 {
                "1 day".to_string()
            } else {
                format!("{d} days")
            };
            (
                format!("{verb} until {} · {days}", pretty_date(until)),
                true,
            )
        }
        _ => (format!("{verb} closed {}", pretty_date(until)), false),
    })
}

/// A row naming a notice.
fn notice_row(n: &Notice, today: &str, go: Card) -> Block {
    Block::Row {
        marker: if n.open_on(today) {
            Marker::Open
        } else {
            Marker::Closed
        },
        text: format!("{} · {}", pretty_date(&n.date), n.title),
        go,
    }
}

/// A row naming a council item.
fn council_row(c: &cartopolis_core::plans::CouncilItem, go: Card) -> Block {
    let state = if c.decided {
        "decided"
    } else {
        "on the agenda"
    };
    Block::Row {
        marker: Marker::Council,
        text: format!("{} · {state} · {}", pretty_date(c.last_date()), c.title),
        go,
    }
}

/// A row naming a work.
fn work_row(w: &Work, go: Card) -> Block {
    let mut text = format!("{} · {}", w.year, w.measure);
    if !w.place.is_empty() {
        text.push_str(&format!(" · {}", w.place));
    }
    Block::Row {
        marker: Marker::Work,
        text,
        go,
    }
}

/// How a line on a card is coloured (`lib_plans::ink` says how).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ink {
    Strong,
    Text,
    Dim,
    /// A response period that still runs.
    Open,
    Council,
    Work,
}

/// How large a line on a card is set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TextSize {
    Sub,
    Tiny,
}

/// The marker beside a row: a notice's dot, open or closed, a council
/// item's square, a work's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Marker {
    Open,
    Closed,
    Council,
    Work,
}

impl Marker {
    #[must_use]
    pub fn colour(self) -> theme::Rgba8 {
        match self {
            Marker::Open => theme::BRAND,
            Marker::Closed => theme::DARK.accent,
            Marker::Council => theme::COUNCIL,
            Marker::Work => WORK,
        }
    }
}

/// One piece of a card: the card is worked out here
/// ([`Plans::card_model`]) and the library only sets it.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    /// The card's main line.
    Title(String),
    /// A line of text; `gap` leaves a little room above it.
    Line {
        text: String,
        ink: Ink,
        size: TextSize,
        gap: bool,
    },
    /// A link out; with a `note` it is the notice's own (an icon before it,
    /// its id after), else a bare link in a list.
    Link {
        label: String,
        url: String,
        note: Option<String>,
    },
    /// A council meeting: its date and its page.
    Meeting {
        date: String,
        label: String,
        url: String,
    },
    /// A heading over rows.
    Section(String),
    /// A row that opens another card.
    Row {
        marker: Marker,
        text: String,
        go: Card,
    },
    /// Follow this place for new plans, or stop.
    Follow { at: (f64, f64), followed: bool },
}

/// What a tap on a card asked for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CardAct {
    /// Open another card.
    Go(Card),
    /// Follow the place, or stop following it.
    Follow((f64, f64)),
}

/// A card: its kicker, and its blocks — `None`
/// while the feed is still being read.
#[derive(Clone, Debug, PartialEq)]
pub struct CardModel {
    pub kicker: Option<(String, Ink)>,
    pub blocks: Option<Vec<Block>>,
}

/// The tip on Follow.
#[must_use]
pub fn follow_tip() -> String {
    format!(
        "Be told when something new is published within {HERE_M:.0} m, also while the app is closed"
    )
}

/// What following does, under the button once it is on.
#[must_use]
pub fn follow_explained() -> String {
    format!(
        "New notices, and works new to the city's programme, within {HERE_M:.0} m are shown at the top of the map, and wake the phone app when it is closed."
    )
}

/// The card a tap on a dot, a square or the ring's *Plans here* opens:
/// whether it stands aside (the place card's rules, for its reasons), for
/// the library's card (`lib_plans`); and seeing a followed place's card
/// counts what is near it as shown.
pub fn drive_plans_card(
    mut plans: ResMut<Plans>,
    metrics: Res<crate::systems::ui_input::UiMetrics>,
    right_panel: Res<crate::resources::RightPanel>,
    menu: Res<crate::systems::ui::MainMenu>,
    driver: Res<crate::systems::driver::DriverState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    mut store: ResMut<crate::systems::user_store::UserStore>,
) {
    let Some(card) = plans.card else {
        return;
    };
    let hidden = driver.active
        || keyboard.is_open()
        || menu.open
        || (metrics.compact() && !right_panel.is(crate::resources::RightPanel::None));
    if !hidden {
        if let (Card::Here { lat, lng }, Some(feed)) = (card, plans.feed.clone()) {
            see_followed(&mut store, (lat, lng), &feed);
        }
    }
    if plans.lib_view.card_hidden != hidden {
        plans.lib_view.card_hidden = hidden;
    }
}

impl Plans {
    /// The card `card`, as the library draws it.
    #[must_use]
    pub fn card_model(&self, card: Card, dl: &DataLayers, follow: &PlansFollow) -> CardModel {
        let now = now_unix();
        let today = civil_date(now);
        let since = dl.plans_period.since(now);
        let feed = self.feed.as_deref();
        let kicker = match (card, feed) {
            (Card::Notice(i), Some(f)) => f.notices.get(i).map(|n| {
                let ink = if n.open_on(&today) {
                    Ink::Open
                } else {
                    Ink::Dim
                };
                (
                    format!("{} · {}", kind_label(n.kind), pretty_date(&n.date)),
                    ink,
                )
            }),
            (Card::Council(i), Some(f)) => f.council.get(i).map(|c| {
                let state = if c.decided {
                    "decided"
                } else {
                    "on the agenda"
                };
                (
                    format!("Council · {state} · {}", pretty_date(c.last_date())),
                    Ink::Council,
                )
            }),
            (Card::Work(i), Some(f)) => f.works.get(i).map(|w| {
                (
                    format!("Works · {} · {}", work_label(w.kind), w.year),
                    Ink::Work,
                )
            }),
            (Card::Here { .. }, _) => Some(("Plans here".to_string(), Ink::Dim)),
            _ => None,
        };
        let blocks = feed.map(|feed| match card {
            Card::Notice(i) => notice_blocks(feed, i, &today),
            Card::Council(i) => council_blocks(feed, i, &today),
            Card::Work(i) => work_blocks(feed, i, &today),
            Card::Here { lat, lng } => {
                let mut blocks = here_blocks(feed, (lat, lng), dl, &today, &since);
                blocks.push(Block::Follow {
                    at: (lat, lng),
                    followed: follow.at((lat, lng)).is_some(),
                });
                blocks
            }
        });
        CardModel { kicker, blocks }
    }

    /// Do what a tap on the card asked: open another card, or follow the
    /// place (or stop).
    pub fn act(&mut self, a: CardAct, store: &mut crate::systems::user_store::UserStore) {
        match a {
            CardAct::Go(card) => self.card = Some(card),
            CardAct::Follow(at) => {
                store.data.plans_follow.toggle(at, self.feed.as_deref());
                store.dirty = true;
            }
        }
    }

    /// The news chip's two lines — how many and where, and the newest one —
    /// while there is news.
    #[must_use]
    pub fn news_lines(&self) -> Option<(String, String)> {
        let feed = self.feed.as_deref()?;
        let &(first, place) = self.news.first()?;
        let line = match first {
            Card::Notice(i) => feed.notices.get(i).map(|n| {
                let mut line = format!("{}: {}", kind_label(n.kind), n.title);
                if !n.place.is_empty() {
                    line.push_str(&format!(" · {}", n.place));
                }
                line
            }),
            Card::Work(i) => feed.works.get(i).map(|w| {
                let mut line = format!("Works {}: {}", w.year, w.measure);
                if !w.place.is_empty() {
                    line.push_str(&format!(" · {}", w.place));
                }
                line
            }),
            _ => None,
        }?;
        let count = self.news.len();
        let head = if count == 1 {
            "New near a place you follow".to_string()
        } else if self.news.iter().all(|(_, p)| *p == place) {
            format!("{count} new near a place you follow")
        } else {
            format!("{count} new near places you follow")
        };
        Some((head, line))
    }

    /// The news chip tapped: the newest one's own card, and everything near
    /// its place counted as shown.
    pub fn open_news(&mut self, store: &mut crate::systems::user_store::UserStore) {
        let (Some(feed), Some(&(first, place))) = (self.feed.clone(), self.news.first()) else {
            return;
        };
        store.data.plans_follow.mark_seen(place, &feed);
        store.dirty = true;
        self.news.retain(|(_, p)| *p != place);
        self.card = Some(first);
    }
}

/// A notice: what, where, until when, the link, the council items naming its
/// place, and other notices on the same spot.
fn notice_blocks(feed: &PlansFeed, i: usize, today: &str) -> Vec<Block> {
    let Some(n) = feed.notices.get(i) else {
        return Vec::new();
    };
    let mut out = vec![Block::Title(n.title.clone())];
    if !n.place.is_empty() {
        out.push(Block::Line {
            text: n.place.clone(),
            ink: Ink::Dim,
            size: TextSize::Sub,
            gap: false,
        });
    }
    if let Some((line, running)) = deadline(n, today) {
        out.push(Block::Line {
            text: line,
            ink: if running { Ink::Open } else { Ink::Dim },
            size: TextSize::Sub,
            gap: true,
        });
    }
    out.push(Block::Link {
        label: "Read the notice".to_string(),
        url: notice_url(&n.id),
        note: Some(n.id.clone()),
    });
    if !n.council.is_empty() {
        out.push(Block::Section(
            "In the council · matched by place name".to_string(),
        ));
        for &c in &n.council {
            if let Some(item) = feed.council.get(c as usize) {
                out.push(council_row(item, Card::Council(c as usize)));
            }
        }
    }
    let also: Vec<(usize, &Notice)> = feed
        .notices
        .iter()
        .enumerate()
        .filter(|(j, o)| *j != i && metres((n.lat, n.lng), (o.lat, o.lng)) <= ALSO_HERE_M)
        .take(3)
        .collect();
    if !also.is_empty() {
        out.push(Block::Section("Also here".to_string()));
        for (j, o) in also {
            out.push(notice_row(o, today, Card::Notice(j)));
        }
    }
    out
}

/// A work: the programme's own words, a plain sentence for the measure, what
/// the programme does not say, the council business naming its street, and
/// the other works on that street.
fn work_blocks(feed: &PlansFeed, i: usize, today: &str) -> Vec<Block> {
    let Some(w) = feed.works.get(i) else {
        return Vec::new();
    };
    let mut out = vec![Block::Title(format!("“{}”", w.measure))];
    if !w.place.is_empty() {
        out.push(Block::Line {
            text: w.place.clone(),
            ink: Ink::Dim,
            size: TextSize::Sub,
            gap: false,
        });
    }
    if let Some(line) = gloss(&w.measure) {
        out.push(Block::Line {
            text: line.to_string(),
            ink: Ink::Text,
            size: TextSize::Sub,
            gap: true,
        });
    }
    out.push(Block::Line {
        text: format!(
            "Planned for {}. The programme names the year only: no dates, closures or detours.",
            w.year
        ),
        ink: Ink::Work,
        size: TextSize::Sub,
        gap: true,
    });
    if !w.council.is_empty() {
        out.push(Block::Section(
            "In the council · matched by place name".to_string(),
        ));
        for &c in &w.council {
            if let Some(item) = feed.council.get(c as usize) {
                out.push(council_row(item, Card::Council(c as usize)));
            }
        }
    }
    if !w.place.is_empty() {
        let also: Vec<(usize, &Work)> = feed
            .works
            .iter()
            .enumerate()
            .filter(|(j, o)| *j != i && o.place == w.place && current(o, today))
            .take(4)
            .collect();
        if !also.is_empty() {
            out.push(Block::Section(format!("Also planned on {}", w.place)));
            for (j, o) in also {
                out.push(work_row(o, Card::Work(j)));
            }
        }
    }
    out.push(Block::Line {
        text: format!("Gemeente Groningen · maintenance programme {}", w.year),
        ink: Ink::Dim,
        size: TextSize::Tiny,
        gap: true,
    });
    out
}

/// A council item: its meetings, its documents, and the notices published
/// where its title points.
fn council_blocks(feed: &PlansFeed, i: usize, today: &str) -> Vec<Block> {
    let Some(c) = feed.council.get(i) else {
        return Vec::new();
    };
    let mut out = vec![Block::Title(c.title.clone())];
    let names: Vec<String> = c.places.iter().map(|p| format!("“{}”", p.name)).collect();
    out.push(Block::Line {
        text: format!("Placed by the name {} in its title", names.join(", ")),
        ink: Ink::Council,
        size: TextSize::Tiny,
        gap: false,
    });
    out.push(Block::Section("Meetings".to_string()));
    for s in &c.steps {
        out.push(Block::Meeting {
            date: pretty_date(&s.date),
            label: s.body.clone(),
            url: s.url.clone(),
        });
    }
    if !c.documents.is_empty() {
        out.push(Block::Section("Documents".to_string()));
        for d in c.documents.iter().take(6) {
            out.push(Block::Link {
                label: d.name.clone(),
                url: d.url.clone(),
                note: None,
            });
        }
        if c.documents.len() > 6 {
            out.push(Block::Line {
                text: format!("and {} more on the meeting pages", c.documents.len() - 6),
                ink: Ink::Dim,
                size: TextSize::Tiny,
                gap: false,
            });
        }
    }
    let linked: Vec<(usize, &Notice)> = feed
        .notices
        .iter()
        .enumerate()
        .filter(|(_, n)| n.council.contains(&(i as u32)))
        .take(4)
        .collect();
    if !linked.is_empty() {
        out.push(Block::Section("Published at the same place".to_string()));
        for (j, n) in linked {
            out.push(notice_row(n, today, Card::Notice(j)));
        }
    }
    out
}

/// Seeing a followed place's card counts what is near it as shown.
fn see_followed(
    store: &mut crate::systems::user_store::UserStore,
    at: (f64, f64),
    feed: &PlansFeed,
) {
    let follow = &store.data.plans_follow;
    if let Some(i) = follow.at(at) {
        let place = follow.places[i];
        if follow.news(feed).iter().any(|(_, p)| *p == place) {
            store.data.plans_follow.mark_seen(place, feed);
            store.dirty = true;
        }
    }
}

/// Whether the chip at the top for a followed place's news stands aside (a
/// card, a panel, the menu, driving), for the library's chip (`lib_plans`).
pub fn drive_plans_news(
    mut plans: ResMut<Plans>,
    right_panel: Res<crate::resources::RightPanel>,
    menu: Res<crate::systems::ui::MainMenu>,
    driver: Res<crate::systems::driver::DriverState>,
) {
    let hidden = plans.card.is_some()
        || driver.active
        || menu.open
        || !right_panel.is(crate::resources::RightPanel::None);
    if plans.lib_view.news_hidden != hidden {
        plans.lib_view.news_hidden = hidden;
    }
}

/// Everything published within [`HERE_M`] of a spot, open first.
fn here_blocks(
    feed: &PlansFeed,
    at: (f64, f64),
    dl: &DataLayers,
    today: &str,
    since: &str,
) -> Vec<Block> {
    let mut near: Vec<(usize, &Notice)> = feed
        .notices
        .iter()
        .enumerate()
        .filter(|(_, n)| n.date.as_str() >= since && metres(at, (n.lat, n.lng)) <= HERE_M)
        .collect();
    near.sort_by_key(|(_, n)| !n.open_on(today));
    let open = near.iter().filter(|(_, n)| n.open_on(today)).count();
    let since_label = match dl.plans_period {
        Period::ThreeMonths => "in three months",
        _ => "in a year",
    };
    let summary = if near.is_empty() {
        format!("Nothing published within {HERE_M:.0} m {since_label}.")
    } else {
        format!(
            "{} within {HERE_M:.0} m {since_label} · {open} still open",
            near.len()
        )
    };
    let mut out = vec![Block::Line {
        text: summary,
        ink: Ink::Strong,
        size: TextSize::Sub,
        gap: false,
    }];
    for (j, n) in near.iter().take(6) {
        out.push(notice_row(n, today, Card::Notice(*j)));
    }
    let works: Vec<(usize, &Work)> = feed
        .works
        .iter()
        .enumerate()
        .filter(|(_, w)| current(w, today) && w.distance_m(at) <= HERE_M)
        .take(4)
        .collect();
    if !works.is_empty() {
        out.push(Block::Section("Works planned here".to_string()));
        for (j, w) in works {
            out.push(work_row(w, Card::Work(j)));
        }
    }
    if near.len() > 6 && !dl.plans_enabled {
        out.push(Block::Line {
            text: "Turn on the Plans layer (Layers, Places) to see them all.".to_string(),
            ink: Ink::Dim,
            size: TextSize::Tiny,
            gap: false,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An outline is cut where it leaves the map, and one that misses it
    /// draws nothing.
    #[test]
    fn outlines_are_cut_to_the_map() {
        let map = Rect::new(0.0, 0.0, 100.0, 100.0);
        let (t0, t1) = clip_params(Vec2::new(-50.0, 50.0), Vec2::new(150.0, 50.0), map).unwrap();
        assert!((t0 - 0.25).abs() < 1e-5 && (t1 - 0.75).abs() < 1e-5);
        assert_eq!(
            clip_params(Vec2::new(10.0, 10.0), Vec2::new(20.0, 20.0), map),
            Some((0.0, 1.0))
        );
        assert!(clip_params(Vec2::new(-10.0, -10.0), Vec2::new(-1.0, 200.0), map).is_none());
        assert!(clip_params(Vec2::new(f32::NAN, 0.0), Vec2::new(1.0, 1.0), map).is_none());
    }

    /// The dash pattern runs on round a corner rather than starting again,
    /// and only the dashes on the map are made.
    #[test]
    fn dashes_run_on_round_corners() {
        let map = Rect::new(-1000.0, -1000.0, 1000.0, 1000.0);
        // 8 along, then down: dash 6, gap 4 — the second dash starts 2 past
        // the corner.
        let pts = [Vec2::ZERO, Vec2::new(8.0, 0.0), Vec2::new(8.0, 20.0)];
        let d = dashes(&pts, 6.0, 4.0, map);
        let near = |a: Vec2, b: Vec2| a.distance(b) < 1e-4;
        assert_eq!(d.len(), 3);
        assert_eq!((d[0].0, d[0].1), (0, 0));
        assert!(near(d[0].2, Vec2::ZERO) && near(d[0].3, Vec2::new(6.0, 0.0)));
        assert_eq!((d[1].0, d[1].1), (1, 1));
        assert!(near(d[1].2, Vec2::new(8.0, 2.0)) && near(d[1].3, Vec2::new(8.0, 8.0)));
        // A path far longer than the map makes only the map's share.
        let long = [Vec2::new(-1.0e6, 0.0), Vec2::new(1.0e6, 0.0)];
        let n = dashes(&long, 6.0, 4.0, map).len();
        assert!((195..=205).contains(&n), "{n} dashes");
    }

    #[test]
    fn dates_read_as_words() {
        assert_eq!(pretty_date("2026-10-28"), "28 Oct 2026");
        assert_eq!(pretty_date("2026-02-05"), "5 Feb 2026");
        assert_eq!(pretty_date("soon"), "soon");
    }

    #[test]
    fn a_deadline_counts_down_and_then_closes() {
        let mut n = Notice {
            id: "gmb-2026-501719".into(),
            kind: NoticeKind::Application,
            title: String::new(),
            place: String::new(),
            date: "2026-10-28".into(),
            respond_until: Some("2026-12-07".into()),
            lat: 53.2169,
            lng: 6.5626,
            area: vec![],
            council: vec![],
        };
        assert_eq!(
            deadline(&n, "2026-10-30"),
            Some(("Responses until 7 Dec 2026 · 38 days".into(), true))
        );
        n.kind = NoticeKind::Decision;
        assert_eq!(
            deadline(&n, "2026-12-08"),
            Some(("Objections closed 7 Dec 2026".into(), false))
        );
        n.respond_until = None;
        assert_eq!(deadline(&n, "2026-10-30"), None);
    }

    #[test]
    fn kinds_survive_the_prefs_and_default_when_missing() {
        let mut kinds = default_kinds();
        assert!(!kinds[3] && !kinds[4], "events and traffic start off");
        kinds[0] = false;
        let saved = kinds_to_prefs(&kinds);
        assert_eq!(kinds_from_prefs(&saved), kinds);
        assert_eq!(
            kinds_from_prefs(&std::collections::HashMap::new()),
            default_kinds()
        );
    }

    #[test]
    fn the_period_reaches_back() {
        // 2026-10-30.
        let now = 1_793_318_400;
        assert_eq!(Period::ThreeMonths.since(now), "2026-07-30");
        assert_eq!(Period::Year.since(now), "2025-10-29");
        assert_eq!(Period::from_key("1y"), Period::Year);
        assert_eq!(Period::from_key("?"), Period::ThreeMonths);
    }

    #[test]
    fn the_ring_offers_plans_in_groningen_only() {
        assert!(covers(53.2172, 6.5633), "the Vismarkt");
        assert!(covers(53.169, 6.604), "Haren");
        assert!(!covers(52.3731, 4.8922), "Amsterdam");
    }

    #[test]
    fn metres_at_city_scale() {
        // The Akerkhof and the Zwanestraat tree are ~170 m apart.
        let d = metres((53.2169, 6.5626), (53.218_354, 6.563_906));
        assert!((150.0..200.0).contains(&d), "{d}");
    }

    fn notice(id: &str, lat: f64, lng: f64) -> Notice {
        Notice {
            id: id.into(),
            kind: NoticeKind::Application,
            title: "kappen van 1 boom".into(),
            place: "Zwanestraat 27".into(),
            date: "2026-11-17".into(),
            respond_until: None,
            lat,
            lng,
            area: Vec::new(),
            council: Vec::new(),
        }
    }

    #[test]
    fn a_followed_place_has_news_only_of_what_came_after() {
        let zwanestraat = (53.218_354, 6.563_906);
        let mut feed = PlansFeed {
            notices: vec![
                notice("gmb-1", zwanestraat.0, zwanestraat.1),
                notice("gmb-far", 53.2169, 6.5626),
            ],
            ..PlansFeed::default()
        };
        let mut follow = PlansFollow::default();
        follow.toggle(zwanestraat, Some(&feed));
        // What was there when it was followed is not news.
        assert!(follow.news(&feed).is_empty());
        // A tap a few metres off is the same place.
        assert_eq!(follow.at((53.21837, 6.56392)), Some(0));

        // A notice 40 m away is; one 170 m away is not.
        feed.notices
            .insert(0, notice("gmb-2", zwanestraat.0 + 0.000_36, zwanestraat.1));
        feed.notices.insert(0, notice("gmb-3", 53.2169, 6.5626));
        let news = follow.news(&feed);
        let place = [zwanestraat.0, zwanestraat.1];
        assert_eq!(news, vec![(Card::Notice(1), place)]);

        // A work reaching within 100 m is news too, after the notices.
        feed.works.push(Work {
            id: "wegen_mjp_2027-1".into(),
            kind: WorkKind::Road,
            measure: "Herstraten".into(),
            year: 2027,
            place: "Zwanestraat".into(),
            lat: 53.2200,
            lng: zwanestraat.1,
            areas: vec![],
            lines: vec![vec![
                [53.2200, zwanestraat.1],
                [zwanestraat.0 + 0.000_6, zwanestraat.1],
            ]],
            points: vec![],
            council: vec![],
        });
        let news = follow.news(&feed);
        assert_eq!(news, vec![(Card::Notice(1), place), (Card::Work(0), place)]);

        // Shown, it is not news any more, and only ids still in the feed are kept.
        follow.seen.push("gmb-gone".into());
        follow.mark_seen(news[0].1, &feed);
        assert!(follow.news(&feed).is_empty());
        assert!(!follow.seen.contains(&"gmb-gone".to_string()));
        assert!(follow.seen.contains(&"wegen_mjp_2027-1".to_string()));

        // Stopping is a second tap.
        follow.toggle(zwanestraat, Some(&feed));
        assert!(follow.places.is_empty());
    }
}
