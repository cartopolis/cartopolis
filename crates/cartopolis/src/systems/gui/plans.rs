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
//! **Painted, not meshed**, for the reason saved places are
//! (`systems::place_pins`): a notice is a remark *about* a spot, it has to be
//! findable from the street and from a few kilometres up, and so it is a dot
//! of fixed screen size over the map. An orange dot's response period still
//! runs; a grey one is history. A council item is a blue square on the place
//! its title names.
//!
//! **What is drawn** is chosen in the tile's ⋯: the kinds of notice, whether
//! the council is shown, and how far back ([`Period`]). With the layer off
//! nothing is fetched or drawn, and the map ring's *Plans here* still answers
//! for one spot — it loads the feed for that.
//!
//! Three systems: [`drive_plans`] fetches and takes the ring's request,
//! [`render_plans`] draws the dots in the panel chain (it culls against the
//! map area, as the pins do), and [`render_plans_card`] is the card a tap
//! opens, a floating `Area` like the place card.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use big_space::prelude::GridCell;
use cartopolis_core::plans::{
    civil_date, days_between, metres, near_followed, notice_url, Notice, NoticeKind, PlansFeed,
    NEAR_M as HERE_M,
};
use web_time::{Instant, SystemTime, UNIX_EPOCH};

use crate::components::FreeCamera;
use crate::grid::anchored_position_f64;
use crate::platform::{http, storage};
use crate::resources::MapAnchor;
use crate::systems::data_layers::DataLayers;
use crate::systems::egui_manager::{EguiMut, EguiRef, WorldAnchored};
use crate::systems::globe::GlobeState;
use crate::systems::place_pins::{map_area, place_delta};
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
/// is one orange smudge from higher, and every dot is a live `Area`.
const MAX_ALTITUDE_M: f64 = 12_000.0;
/// At most this many dots a frame, the newest first.
const MAX_DOTS: usize = 400;
/// A dot's radius, points; an open one's.
const DOT_R: f32 = 5.5;
const OPEN_R: f32 = 6.5;
/// The council square's half-size, points.
const COUNCIL_R: f32 = 9.0;
/// How near counts as "here" for a card's "Also here", metres. The ring's
/// *Plans here* and a followed place use the shared `HERE_M`.
const ALSO_HERE_M: f64 = 40.0;
/// Width of the card: fixed, as the place card's is.
const CARD_WIDTH: f32 = 330.0;

/// How far back the layer looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Period {
    /// Only notices whose response period still runs.
    Open,
    #[default]
    ThreeMonths,
    Year,
}

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
pub const KINDS: [(NoticeKind, &str, bool); 6] = [
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
    /// Notices near a followed place that were there when it was followed or
    /// have been shown since, by id.
    #[serde(default)]
    pub seen: Vec<String>,
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
            let near = feed.notices.iter().filter(|n| near_followed(place, n));
            self.seen.extend(near.map(|n| n.id.clone()));
        }
    }

    /// Notices near a followed place not shown yet, newest first: the index
    /// in the feed and the place it is near.
    #[must_use]
    pub fn news(&self, feed: &PlansFeed) -> Vec<(usize, [f64; 2])> {
        if self.places.is_empty() {
            return Vec::new();
        }
        let seen: std::collections::HashSet<&str> = self.seen.iter().map(String::as_str).collect();
        feed.notices
            .iter()
            .enumerate()
            .filter(|(_, n)| !seen.contains(n.id.as_str()))
            .filter_map(|(i, n)| {
                self.places
                    .iter()
                    .find(|p| near_followed(**p, n))
                    .map(|p| (i, *p))
            })
            .collect()
    }

    /// Count everything near `place` as shown, and forget shown notices the
    /// feed no longer holds, so the list does not grow for ever.
    pub fn mark_seen(&mut self, place: [f64; 2], feed: &PlansFeed) {
        let near = feed.notices.iter().filter(|n| near_followed(place, n));
        self.seen.extend(near.map(|n| n.id.clone()));
        let ids: std::collections::HashSet<&str> =
            feed.notices.iter().map(|n| n.id.as_str()).collect();
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
    /// Dots drawn last frame, and how many of them still open: the chip.
    shown: (usize, usize),
    /// An agent asked about notices (`mcp::world`): read the feed as if the
    /// layer were on, without drawing it on the person's map.
    pub agent_wants: bool,
    /// Notices near a followed place not shown yet ([`PlansFollow::news`]).
    news: Vec<(usize, [f64; 2])>,
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
                    "plans feed loaded"
                );
                plans.feed = Some(Arc::new(feed));
                plans.loaded_at = Some(Instant::now());
                plans.failed_at = None;
            }
            None => plans.failed_at = Some(Instant::now()),
        }
        // A card pointing into a feed that was replaced points at something else.
        if matches!(plans.card, Some(Card::Notice(_) | Card::Council(_))) {
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
            cartopolis_core::NetMessage::FollowPlans {
                places: places.clone(),
            },
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

/// The dots, areas and council squares over the map, and the chip saying how
/// many. In the panel chain, before the pins, for the reason they are there.
#[allow(clippy::too_many_arguments)]
pub fn render_plans(
    mut egui: EguiMut,
    mut plans: ResMut<Plans>,
    mut dl: ResMut<DataLayers>,
    globe: Res<GlobeState>,
    camera: Query<(&Camera, &GridCell, &Transform), With<FreeCamera>>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    map_anchor: Res<MapAnchor>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    route: Res<crate::systems::navigation::RouteState>,
    driver: Res<crate::systems::driver::DriverState>,
    right_panel: Res<crate::resources::RightPanel>,
) {
    let mut anchored = WorldAnchored::new(egui.ctx(), "plans");
    plans.shown = (0, 0);
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

    let map_rect = map_area(&mut egui, &keyboard, &route);
    let ui_scale = egui.ui_scale();
    let to_ui = |p: Vec2| egui::pos2(p.x / ui_scale, p.y / ui_scale);
    let ctx = egui.ctx_mut().clone();
    let cam_world = anchored_position_f64(cam_cell, cam_tf.translation);
    // This frame's camera basis, not last frame's (`place_pins` says why).
    let cam_view = GlobalTransform::from(Transform::from_rotation(cam_tf.rotation));
    let project = |lat: f64, lng: f64| -> Option<egui::Pos2> {
        let delta = place_delta(&globe, anchor, cam_world, in_space, lat, lng, &terrain)?;
        let screen = cam.world_to_viewport(&cam_view, delta.as_vec3()).ok()?;
        Some(to_ui(screen))
    };

    let now = now_unix();
    let today = civil_date(now);
    let since = dl.plans_period.since(now);
    let mut tapped: Option<Card> = None;
    let (mut drawn, mut open) = (0usize, 0usize);
    if !high {
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Background,
            egui::Id::new("plans_areas"),
        ));
        let painter = painter.with_clip_rect(map_rect);
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
            if !map_rect.contains(tip) || !crate::systems::place_pins::clear_of_chrome(&ctx, tip) {
                continue;
            }
            let is_open = n.open_on(&today);
            drawn += 1;
            open += usize::from(is_open);
            anchored.at(tip);
            // Its area's outline, under the dot, when it was published as one.
            if n.area.len() >= 3 {
                let ring: Option<Vec<egui::Pos2>> =
                    n.area.iter().map(|p| project(p[0], p[1])).collect();
                if let Some(mut ring) = ring {
                    ring.push(ring[0]);
                    let colour = if is_open {
                        theme::BRAND
                    } else {
                        theme::DARK.accent
                    };
                    painter.extend(egui::Shape::dashed_line(
                        &ring,
                        egui::Stroke::new(2.0, colour),
                        6.0,
                        4.0,
                    ));
                }
            }
            if dot(&ctx, map_rect, i, tip, is_open, &n.title) {
                tapped = Some(Card::Notice(i));
            }
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
                    if !map_rect.contains(at)
                        || !crate::systems::place_pins::clear_of_chrome(&ctx, at)
                    {
                        continue;
                    }
                    anchored.at(at);
                    if council_square(&ctx, map_rect, (i, j), at, c.decided, &c.title) {
                        tapped = Some(Card::Council(i));
                    }
                }
            }
        }
    }
    plans.shown = (drawn, open);
    if let Some(card) = tapped {
        plans.card = Some(card);
    }
    if right_panel.is(crate::resources::RightPanel::None) {
        let top = crate::systems::dock::reserved_top() + theme::OSD_MARGIN + 52.0;
        if chip(&ctx, top, drawn, open, high) {
            dl.plans_enabled = false;
        }
    }
}

/// One notice's dot; whether it was tapped. Its own `Area`, so egui owns the
/// tap and the map's click handlers leave it alone (`place_pins::pin`).
fn dot(
    ctx: &egui::Context,
    map_rect: egui::Rect,
    i: usize,
    at: egui::Pos2,
    open: bool,
    title: &str,
) -> bool {
    let r = if open { OPEN_R } else { DOT_R };
    let side = 2.0 * r + 8.0;
    egui::Area::new(egui::Id::new(("plan_dot", i)))
        .fixed_pos(at)
        .pivot(egui::Align2::CENTER_CENTER)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            ui.set_clip_rect(map_rect);
            let (_, resp) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
            let resp = theme::named(ui, resp, title);
            let r = if resp.hovered() { r * 1.25 } else { r };
            let p = ui.painter();
            // Painted at the exact projected point, not the rounded rect.
            if open {
                p.circle(
                    at,
                    r,
                    theme::BRAND,
                    egui::Stroke::new(2.0, theme::DARK.cream),
                );
            } else {
                p.circle(
                    at,
                    r,
                    theme::DARK.accent,
                    egui::Stroke::new(2.0, theme::DARK.ink),
                );
            }
            resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        })
        .inner
        .clicked()
}

/// A council item's square on a place its title names; whether it was tapped.
fn council_square(
    ctx: &egui::Context,
    map_rect: egui::Rect,
    id: (usize, usize),
    at: egui::Pos2,
    decided: bool,
    title: &str,
) -> bool {
    let side = 2.0 * COUNCIL_R + 6.0;
    egui::Area::new(egui::Id::new(("plan_council", id)))
        .fixed_pos(at)
        .pivot(egui::Align2::CENTER_CENTER)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            ui.set_clip_rect(map_rect);
            let (_, resp) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
            let resp = theme::named(ui, resp, title);
            let r = if resp.hovered() {
                COUNCIL_R * 1.2
            } else {
                COUNCIL_R
            };
            let rect = egui::Rect::from_center_size(at, egui::vec2(2.0 * r, 2.0 * r));
            let p = ui.painter();
            p.rect(
                rect,
                egui::CornerRadius::same(5),
                theme::COUNCIL,
                egui::Stroke::new(2.0, theme::DARK.cream),
                egui::StrokeKind::Middle,
            );
            // A tick for a decision taken, a dot for business still open.
            let glyph = if decided { icons::CHECK } else { icons::DOT };
            p.text(
                at,
                egui::Align2::CENTER_CENTER,
                glyph,
                egui::FontId::proportional(r * 1.3),
                theme::DARK.cream,
            );
            resp.on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text("Council business that names this place")
        })
        .inner
        .clicked()
}

/// The chip at the top while the layer is on: how many notices are drawn and
/// how many still open. Its ✕ turns the layer off; whether it was pressed.
fn chip(ctx: &egui::Context, top: f32, drawn: usize, open: usize, high: bool) -> bool {
    let mut off = false;
    egui::Area::new(egui::Id::new("plans_chip"))
        .order(egui::Order::Middle)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top))
        .show(ctx, |ui| {
            crate::systems::chats::glass_frame()
                .corner_radius(egui::CornerRadius::same(16))
                .inner_margin(egui::Margin::symmetric(10, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.label(
                            egui::RichText::new(icons::FLAG)
                                .size(theme::SIZE_SUB)
                                .color(theme::p().cream),
                        );
                        ui.label(
                            egui::RichText::new("Plans")
                                .size(theme::SIZE_SUB)
                                .color(theme::p().cream),
                        );
                        let what = if high {
                            "zoom in to see them".to_string()
                        } else {
                            format!("{drawn} here")
                        };
                        ui.label(
                            egui::RichText::new(what)
                                .size(theme::SIZE_SUB)
                                .color(theme::p().text_dim),
                        );
                        if !high && open > 0 {
                            ui.label(
                                egui::RichText::new(format!("{open} still open"))
                                    .size(theme::SIZE_SUB)
                                    .color(theme::BRAND),
                            );
                        }
                        off = crate::systems::chats::circle_button(
                            ui,
                            icons::CLEAR,
                            false,
                            true,
                            "Turn the Plans layer off",
                        )
                        .clicked();
                    });
                });
        });
    off
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

/// A link: the system browser where there is one, egui's hyperlink otherwise.
fn link(ui: &mut egui::Ui, label: &str, url: &str) {
    if crate::systems::data_layers::open_url_supported() {
        if ui
            .link(egui::RichText::new(label).size(theme::SIZE_SUB))
            .on_hover_text(url)
            .clicked()
        {
            crate::systems::data_layers::open_url(url);
        }
    } else {
        ui.hyperlink_to(egui::RichText::new(label).size(theme::SIZE_SUB), url);
    }
}

/// A row naming a notice, with its dot; whether it was tapped.
fn notice_row(ui: &mut egui::Ui, n: &Notice, today: &str) -> bool {
    let open = n.open_on(today);
    ui.horizontal(|ui| {
        let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), egui::Sense::hover());
        let colour = if open {
            theme::BRAND
        } else {
            theme::DARK.accent
        };
        ui.painter().circle_filled(dot.center(), 4.0, colour);
        let text = format!("{} · {}", pretty_date(&n.date), n.title);
        ui.add(
            egui::Label::new(egui::RichText::new(text).size(theme::SIZE_SUB))
                .truncate()
                .sense(egui::Sense::click()),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    })
    .inner
}

/// A row naming a council item; whether it was tapped.
fn council_row(ui: &mut egui::Ui, c: &cartopolis_core::plans::CouncilItem) -> bool {
    ui.horizontal(|ui| {
        let (sq, _) = ui.allocate_exact_size(egui::vec2(12.0, 14.0), egui::Sense::hover());
        ui.painter().rect_filled(
            egui::Rect::from_center_size(sq.center(), egui::vec2(10.0, 10.0)),
            3,
            theme::COUNCIL,
        );
        let state = if c.decided {
            "decided"
        } else {
            "on the agenda"
        };
        let text = format!("{} · {state} · {}", pretty_date(c.last_date()), c.title);
        ui.add(
            egui::Label::new(egui::RichText::new(text).size(theme::SIZE_SUB))
                .truncate()
                .sense(egui::Sense::click()),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    })
    .inner
}

fn kicker(ui: &mut egui::Ui, text: &str, colour: egui::Color32) {
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .size(theme::SIZE_TINY)
            .color(colour),
    );
}

fn section(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(text)
            .size(theme::SIZE_TINY)
            .color(theme::p().text_dim),
    );
}

/// The card a tap on a dot, a square or the ring's *Plans here* opens.
#[allow(clippy::too_many_arguments)]
pub fn render_plans_card(
    egui: EguiRef,
    mut plans: ResMut<Plans>,
    dl: Res<DataLayers>,
    right_panel: Res<crate::resources::RightPanel>,
    menu: Res<crate::systems::ui::MainMenu>,
    driver: Res<crate::systems::driver::DriverState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    mut store: ResMut<crate::systems::user_store::UserStore>,
) {
    let Some(card) = plans.card else {
        return;
    };
    // The place card's suppression rules, for its reasons.
    if driver.active || keyboard.is_open() || menu.open {
        return;
    }
    if egui.compact() && !right_panel.is(crate::resources::RightPanel::None) {
        return;
    }
    let today = civil_date(now_unix());
    let since = dl.plans_period.since(now_unix());
    let feed = plans.feed.clone();
    let bottom = crate::systems::dock::reserved_bottom() + 8.0;
    let ctx = egui.ctx();
    let mut next: Option<Option<Card>> = None;
    egui::Area::new(egui::Id::new("plans_card"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bottom))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(CARD_WIDTH.min(egui.viewport_pts().x - 2.0 * theme::OSD_MARGIN));
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::close_button(ui).clicked() {
                            next = Some(None);
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            let head = match (card, &feed) {
                                (Card::Notice(i), Some(f)) => f.notices.get(i).map(|n| {
                                    let colour = if n.open_on(&today) {
                                        theme::BRAND
                                    } else {
                                        theme::p().text_dim
                                    };
                                    (
                                        format!(
                                            "{} · {}",
                                            kind_label(n.kind),
                                            pretty_date(&n.date)
                                        ),
                                        colour,
                                    )
                                }),
                                (Card::Council(i), Some(f)) => f.council.get(i).map(|c| {
                                    let state = if c.decided {
                                        "decided"
                                    } else {
                                        "on the agenda"
                                    };
                                    (
                                        format!(
                                            "Council · {state} · {}",
                                            pretty_date(c.last_date())
                                        ),
                                        theme::COUNCIL_LIGHT,
                                    )
                                }),
                                (Card::Here { .. }, _) => {
                                    Some(("Plans here".to_string(), theme::p().text_dim))
                                }
                                _ => None,
                            };
                            if let Some((text, colour)) = head {
                                kicker(ui, &text, colour);
                            }
                        });
                    });
                });
                let Some(feed) = feed.as_deref() else {
                    ui.label(
                        egui::RichText::new("Reading what is published…")
                            .size(theme::SIZE_SUB)
                            .color(theme::p().text_dim),
                    );
                    return;
                };
                match card {
                    Card::Notice(i) => {
                        if let Some(go) = notice_body(ui, feed, i, &today) {
                            next = Some(Some(go));
                        }
                    }
                    Card::Council(i) => {
                        if let Some(go) = council_body(ui, feed, i, &today) {
                            next = Some(Some(go));
                        }
                    }
                    Card::Here { lat, lng } => {
                        if let Some(go) = here_body(ui, feed, (lat, lng), &dl, &today, &since) {
                            next = Some(Some(go));
                        }
                        follow_row(ui, &mut store, (lat, lng), feed);
                    }
                }
            });
        });
    if let Some(next) = next {
        plans.card = next;
    }
}

/// A notice: what, where, until when, the link, the council items naming its
/// place, and other notices on the same spot. The card to go to, if a row was
/// tapped.
fn notice_body(ui: &mut egui::Ui, feed: &PlansFeed, i: usize, today: &str) -> Option<Card> {
    let n = feed.notices.get(i)?;
    let mut go = None;
    ui.label(
        egui::RichText::new(&n.title)
            .size(theme::SIZE_BODY)
            .color(theme::p().cream),
    );
    if !n.place.is_empty() {
        ui.label(
            egui::RichText::new(&n.place)
                .size(theme::SIZE_SUB)
                .color(theme::p().text_dim),
        );
    }
    if let Some((line, running)) = deadline(n, today) {
        ui.add_space(4.0);
        let colour = if running {
            theme::BRAND
        } else {
            theme::p().text_dim
        };
        ui.label(
            egui::RichText::new(line)
                .size(theme::SIZE_SUB)
                .color(colour),
        );
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(icons::OPEN_EXTERNAL)
                .size(theme::SIZE_SUB)
                .color(theme::p().text_dim),
        );
        link(ui, "Read the notice", &notice_url(&n.id));
        ui.label(
            egui::RichText::new(&n.id)
                .size(theme::SIZE_TINY)
                .color(theme::p().text_dim),
        );
    });
    if !n.council.is_empty() {
        section(ui, "In the council · matched by place name");
        for &c in &n.council {
            if let Some(item) = feed.council.get(c as usize) {
                if council_row(ui, item) {
                    go = Some(Card::Council(c as usize));
                }
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
        section(ui, "Also here");
        for (j, o) in also {
            if notice_row(ui, o, today) {
                go = Some(Card::Notice(j));
            }
        }
    }
    go
}

/// A council item: its meetings, its documents, and the notices published
/// where its title points.
fn council_body(ui: &mut egui::Ui, feed: &PlansFeed, i: usize, today: &str) -> Option<Card> {
    let c = feed.council.get(i)?;
    let mut go = None;
    ui.label(
        egui::RichText::new(&c.title)
            .size(theme::SIZE_BODY)
            .color(theme::p().cream),
    );
    let names: Vec<&str> = c.places.iter().map(|p| p.name.as_str()).collect();
    ui.label(
        egui::RichText::new(format!(
            "Placed by the name {} in its title",
            names
                .iter()
                .map(|n| format!("“{n}”"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .size(theme::SIZE_TINY)
        .color(theme::COUNCIL_LIGHT),
    );
    section(ui, "Meetings");
    for s in &c.steps {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(pretty_date(&s.date))
                    .size(theme::SIZE_SUB)
                    .color(theme::p().text_dim),
            );
            link(ui, &s.body, &s.url);
        });
    }
    if !c.documents.is_empty() {
        section(ui, "Documents");
        for d in c.documents.iter().take(6) {
            link(ui, &d.name, &d.url);
        }
        if c.documents.len() > 6 {
            ui.label(
                egui::RichText::new(format!(
                    "and {} more on the meeting pages",
                    c.documents.len() - 6
                ))
                .size(theme::SIZE_TINY)
                .color(theme::p().text_dim),
            );
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
        section(ui, "Published at the same place");
        for (j, n) in linked {
            if notice_row(ui, n, today) {
                go = Some(Card::Notice(j));
            }
        }
    }
    go
}

/// Under *Plans here*: follow this place for new notices, or stop. Seeing a
/// followed place's card counts what is near it as shown.
fn follow_row(
    ui: &mut egui::Ui,
    store: &mut crate::systems::user_store::UserStore,
    at: (f64, f64),
    feed: &PlansFeed,
) {
    let follow = &store.data.plans_follow;
    let followed = follow.at(at);
    if let Some(i) = followed {
        let place = follow.places[i];
        if follow.news(feed).iter().any(|(_, p)| *p == place) {
            store.data.plans_follow.mark_seen(place, feed);
            store.dirty = true;
        }
    }
    ui.add_space(6.0);
    let label = if followed.is_some() {
        "Following · stop"
    } else {
        "Follow this place"
    };
    let pressed = theme::secondary_toggle(ui, icons::FLAG, label, 0.0, followed.is_some())
        .on_hover_text(format!(
            "Be told when something new is published within {HERE_M:.0} m, also while the app is closed"
        ))
        .clicked();
    if pressed {
        store.data.plans_follow.toggle(at, Some(feed));
        store.dirty = true;
    }
    if followed.is_some() || pressed {
        ui.label(
            egui::RichText::new(format!(
                "New notices within {HERE_M:.0} m are shown at the top of the map, and wake the phone app when it is closed."
            ))
            .size(theme::SIZE_TINY)
            .color(theme::p().text_dim),
        );
    }
}

/// The chip at the top while a followed place has news: how many, and a tap
/// opens the newest one's card. Its own `Foreground` area, as the card is.
pub fn render_plans_news(
    egui: EguiRef,
    mut plans: ResMut<Plans>,
    dl: Res<DataLayers>,
    right_panel: Res<crate::resources::RightPanel>,
    menu: Res<crate::systems::ui::MainMenu>,
    driver: Res<crate::systems::driver::DriverState>,
    mut store: ResMut<crate::systems::user_store::UserStore>,
) {
    if plans.news.is_empty() || plans.card.is_some() || driver.active || menu.open {
        return;
    }
    if !right_panel.is(crate::resources::RightPanel::None) {
        return;
    }
    let Some(feed) = plans.feed.clone() else {
        return;
    };
    let (first, place) = plans.news[0];
    let Some(newest) = feed.notices.get(first) else {
        return;
    };
    let count = plans.news.len();
    // Under the layer's own chip when that is up.
    let chip_up = dl.plans_enabled;
    let top = crate::systems::dock::reserved_top()
        + theme::OSD_MARGIN
        + 52.0
        + if chip_up { 40.0 } else { 0.0 };
    let width = CARD_WIDTH.min(egui.viewport_pts().x - 2.0 * theme::OSD_MARGIN);
    let mut open = false;
    egui::Area::new(egui::Id::new("plans_news"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top))
        .show(egui.ctx(), |ui| {
            let r = crate::systems::chats::glass_frame()
                .corner_radius(egui::CornerRadius::same(14))
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_width(width);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(icons::FLAG)
                                .size(theme::SIZE_BODY)
                                .color(theme::BRAND),
                        );
                        ui.vertical(|ui| {
                            let head = if count == 1 {
                                "New near a place you follow".to_string()
                            } else {
                                format!("{count} new near places you follow")
                            };
                            ui.label(
                                egui::RichText::new(head)
                                    .size(theme::SIZE_SUB)
                                    .color(theme::p().cream),
                            );
                            let mut line = format!("{}: {}", kind_label(newest.kind), newest.title);
                            if !newest.place.is_empty() {
                                line.push_str(&format!(" · {}", newest.place));
                            }
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(line)
                                        .size(theme::SIZE_TINY)
                                        .color(theme::p().text_dim),
                                )
                                .truncate(),
                            );
                        });
                    });
                });
            let tap = ui.interact(
                r.response.rect,
                egui::Id::new("plans_news_tap"),
                egui::Sense::click(),
            );
            let tap = theme::named(ui, tap, "New near a place you follow");
            open = tap
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked();
        });
    if open {
        // The newest notice's own card; everything near its place is shown.
        store.data.plans_follow.mark_seen(place, &feed);
        store.dirty = true;
        plans.news.retain(|(_, p)| *p != place);
        plans.card = Some(Card::Notice(first));
    }
}

/// Everything published within [`HERE_M`] of a spot, open first.
fn here_body(
    ui: &mut egui::Ui,
    feed: &PlansFeed,
    at: (f64, f64),
    dl: &DataLayers,
    today: &str,
    since: &str,
) -> Option<Card> {
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
    ui.label(
        egui::RichText::new(summary)
            .size(theme::SIZE_SUB)
            .color(theme::p().cream),
    );
    let mut go = None;
    for (j, n) in near.iter().take(6) {
        if notice_row(ui, n, today) {
            go = Some(Card::Notice(*j));
        }
    }
    if near.len() > 6 && !dl.plans_enabled {
        ui.label(
            egui::RichText::new("Turn on the Plans layer (Layers, Places) to see them all.")
                .size(theme::SIZE_TINY)
                .color(theme::p().text_dim),
        );
    }
    go
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(news, vec![(1, [zwanestraat.0, zwanestraat.1])]);

        // Shown, it is not news any more, and only ids still in the feed are kept.
        follow.seen.push("gmb-gone".into());
        follow.mark_seen(news[0].1, &feed);
        assert!(follow.news(&feed).is_empty());
        assert!(!follow.seen.contains(&"gmb-gone".to_string()));

        // Stopping is a second tap.
        follow.toggle(zwanestraat, Some(&feed));
        assert!(follow.places.is_empty());
    }
}
