//! Waking a closed phone for a new plan near a place its account follows
//! (`FollowPlans`, protocol 50, `docs/notes/plans.md`).
//!
//! Every [`EVERY`] this reads the municipality's notices (atlas's
//! `/plans/groningen.json`, [`feed_url`]), takes the ones it has not checked
//! before, and wakes, through `push`, every account that follows a spot within
//! [`NEAR_M`] of one. The push is the one word "plan" and goes only to phones
//! that registered their address saying they know it (`push::PLANS_PREFIX`);
//! the app, once opened, finds which notice itself, from the same feed.
//!
//! **What was checked is kept** (`plans_seen`), so a restart wakes nobody
//! twice; on a server's first read nothing has been checked and every notice
//! is marked without waking anybody. A notice dated more than [`RECENT_DAYS`]
//! back is old news even when this server has not seen it — a feed rebuilt
//! over a longer period, say — and wakes nobody either.
//!
//! **Works** in the city's maintenance programme wake the same way, by any
//! part of the work within [`NEAR_M`], when new to the programme — next year's
//! — with their own first read marked silently ([`WORKS_READ`]).

use std::collections::BTreeSet;
use std::time::Duration;

use cartopolis_core::plans::{civil_date, near_followed, work_near_followed, PlansFeed, NEAR_M};
use rusqlite::Connection;
use tokio::sync::mpsc;

use crate::config::SimConfig;
use crate::db::{self, SharedConn};
use crate::push::{Kind, Ring};

/// How often the feed is read. Atlas rebuilds it every few hours and the
/// municipality publishes once a working day, so this is about not being late
/// by more than a coffee, not about speed.
const EVERY: Duration = Duration::from_secs(15 * 60);

/// A notice published this many days ago or longer is not news.
const RECENT_DAYS: u64 = 14;

/// Where the feed is read: the configured URL, else atlas's when this server
/// runs one (`cartopolis-server`), else nowhere.
#[must_use]
pub fn feed_url(config: &SimConfig) -> Option<String> {
    config.plans_url.clone().or_else(|| {
        config
            .atlas_url
            .as_deref()
            .map(|a| format!("{}/plans/groningen.json", a.trim_end_matches('/')))
    })
}

/// Start reading the feed, when there is one to read.
pub fn spawn(conn: SharedConn, config: &SimConfig, ring: mpsc::UnboundedSender<Ring>) {
    let Some(url) = feed_url(config) else {
        tracing::info!("no plans feed: nobody is woken for plans near a followed place");
        return;
    };
    tokio::spawn(run(conn, url, ring));
}

async fn run(conn: SharedConn, url: String, ring: mpsc::UnboundedSender<Ring>) {
    let mut tick = tokio::time::interval(EVERY);
    loop {
        tick.tick().await;
        let fetched = {
            let url = url.clone();
            tokio::task::spawn_blocking(move || read_feed(&url)).await
        };
        let feed = match fetched {
            Ok(Ok(feed)) => feed,
            Ok(Err(e)) => {
                tracing::warn!(%url, error = %e, "plans feed not read");
                continue;
            }
            Err(_) => continue,
        };
        let since = civil_date(now_s().saturating_sub(RECENT_DAYS * 86_400));
        let checked = db::call(&conn, move |c| check(c, &feed, &since)).await;
        let members = match checked {
            Ok(Ok(members)) => members,
            Ok(Err(e)) | Err(e) => {
                tracing::warn!(error = %e, "plans not checked against followed places");
                continue;
            }
        };
        if members.is_empty() {
            continue;
        }
        tracing::info!(accounts = members.len(), "new plans near followed places");
        let _ = ring.send(Ring {
            kind: Kind::Plan,
            caller: String::new(),
            caller_name: String::new(),
            members,
        });
    }
}

fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn read_feed(url: &str) -> Result<PlansFeed, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .build();
    let body = agent
        .get(url)
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

/// Mark the feed's unchecked notices and works as checked, and answer the
/// accounts that follow a spot near a new one: a notice published on or after
/// `since` (`YYYY-MM-DD`), or a work new to the programme. Nobody on a
/// server's first read of either.
pub fn check(c: &mut Connection, feed: &PlansFeed, since: &str) -> anyhow::Result<Vec<String>> {
    let ids: Vec<&str> = feed.notices.iter().map(|n| n.id.as_str()).collect();
    let (unseen, checked_before) = db::unseen_notices(c, &ids)?;
    let mut new_notices = Vec::new();
    if !unseen.is_empty() {
        db::mark_notices_seen(c, &unseen)?;
        if checked_before {
            let unseen: BTreeSet<&str> = unseen.iter().map(String::as_str).collect();
            new_notices = feed
                .notices
                .iter()
                .filter(|n| unseen.contains(n.id.as_str()) && n.date.as_str() >= since)
                .collect();
        } else {
            tracing::info!(
                notices = unseen.len(),
                "plans feed read for the first time; nobody woken for these"
            );
        }
    }
    let new_works = new_works(c, feed)?;
    if new_notices.is_empty() && new_works.is_empty() {
        return Ok(Vec::new());
    }
    let accounts: BTreeSet<String> = db::plan_follows(c)?
        .into_iter()
        .filter(|(_, lat, lng)| {
            new_notices.iter().any(|n| near_followed([*lat, *lng], n))
                || new_works
                    .iter()
                    .any(|w| work_near_followed([*lat, *lng], w))
        })
        .map(|(account, _, _)| account)
        .collect();
    tracing::debug!(
        notices = new_notices.len(),
        works = new_works.len(),
        near_m = NEAR_M,
        "plans checked"
    );
    Ok(accounts.into_iter().collect())
}

/// Marked once the maintenance programme has been read, so the first read
/// wakes nobody while next year's programme does: works carry no date to tell
/// old from new by.
const WORKS_READ: &str = "works:read";

/// The feed's works not checked before, now marked; none on the first read.
fn new_works<'f>(
    c: &mut Connection,
    feed: &'f PlansFeed,
) -> anyhow::Result<Vec<&'f cartopolis_core::plans::Work>> {
    if feed.works.is_empty() {
        return Ok(Vec::new());
    }
    let mut ids: Vec<&str> = feed.works.iter().map(|w| w.id.as_str()).collect();
    ids.push(WORKS_READ);
    let (unseen, _) = db::unseen_notices(c, &ids)?;
    if unseen.is_empty() {
        return Ok(Vec::new());
    }
    db::mark_notices_seen(c, &unseen)?;
    if unseen.iter().any(|id| id == WORKS_READ) {
        tracing::info!(
            works = unseen.len() - 1,
            "works programme read for the first time; nobody woken for these"
        );
        return Ok(Vec::new());
    }
    let unseen: BTreeSet<&str> = unseen.iter().map(String::as_str).collect();
    Ok(feed
        .works
        .iter()
        .filter(|w| unseen.contains(w.id.as_str()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_core::plans::{Notice, NoticeKind};

    fn notice(id: &str, date: &str, lat: f64, lng: f64) -> Notice {
        Notice {
            id: id.into(),
            kind: NoticeKind::Application,
            title: "kappen van 1 boom".into(),
            place: String::new(),
            date: date.into(),
            respond_until: None,
            lat,
            lng,
            area: Vec::new(),
            council: Vec::new(),
        }
    }

    fn feed(notices: Vec<Notice>) -> PlansFeed {
        PlansFeed {
            notices,
            ..PlansFeed::default()
        }
    }

    #[test]
    fn a_new_notice_wakes_who_follows_a_spot_near_it_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = db::open_and_migrate(dir.path().join("w.db").to_str().unwrap()).unwrap();
        // Zwanestraat 27, and a spot 400 m off.
        db::set_plan_follows(&mut c, "ann", &[[53.2185, 6.5640]]).unwrap();
        db::set_plan_follows(&mut c, "bob", &[[53.2150, 6.5700]]).unwrap();

        // The first read marks a year of notices and wakes nobody.
        let first = feed(vec![notice("gmb-1", "2026-11-02", 53.2185, 6.5640)]);
        assert!(check(&mut c, &first, "2026-11-03").unwrap().is_empty());

        // A notice 50 m from Ann's spot wakes Ann alone.
        let mut second = first.clone();
        second
            .notices
            .insert(0, notice("gmb-2", "2026-11-17", 53.21895, 6.5640));
        assert_eq!(check(&mut c, &second, "2026-11-03").unwrap(), vec!["ann"]);
        // Read again, it is not news.
        assert!(check(&mut c, &second, "2026-11-03").unwrap().is_empty());

        // An old notice this server had not seen is not news either.
        let mut third = second.clone();
        third
            .notices
            .push(notice("gmb-0", "2026-01-05", 53.2185, 6.5640));
        assert!(check(&mut c, &third, "2026-11-03").unwrap().is_empty());

        // Following nothing any more is following nothing.
        db::set_plan_follows(&mut c, "ann", &[]).unwrap();
        let mut fourth = third.clone();
        fourth
            .notices
            .insert(0, notice("gmb-3", "2026-11-18", 53.2185, 6.5640));
        assert!(check(&mut c, &fourth, "2026-11-03").unwrap().is_empty());
    }

    fn work(id: &str, lat: f64, lng: f64) -> cartopolis_core::plans::Work {
        cartopolis_core::plans::Work {
            id: id.into(),
            kind: cartopolis_core::plans::WorkKind::Sewer,
            measure: "Relinen".into(),
            year: 2027,
            place: String::new(),
            lat,
            lng,
            areas: Vec::new(),
            lines: Vec::new(),
            points: vec![[lat, lng]],
            council: Vec::new(),
        }
    }

    #[test]
    fn a_new_work_wakes_who_follows_near_it_but_the_first_programme_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = db::open_and_migrate(dir.path().join("w.db").to_str().unwrap()).unwrap();
        db::set_plan_follows(&mut c, "ann", &[[53.2300, 6.5750]]).unwrap();
        // Notices were read before works existed in the feed.
        let mut f = feed(vec![notice("gmb-1", "2026-11-02", 53.0, 6.0)]);
        assert!(check(&mut c, &f, "2026-11-03").unwrap().is_empty());

        // The first programme read, with a work on Ann's spot, wakes nobody.
        f.works = vec![work("riolering_mjp_2026-1", 53.2300, 6.5750)];
        assert!(check(&mut c, &f, "2026-11-03").unwrap().is_empty());

        // Next year's work 50 m off wakes Ann, once.
        f.works.push(work("riolering_mjp_2027-1", 53.23045, 6.5750));
        assert_eq!(check(&mut c, &f, "2026-11-03").unwrap(), vec!["ann"]);
        assert!(check(&mut c, &f, "2026-11-03").unwrap().is_empty());

        // One 400 m off wakes nobody.
        f.works.push(work("riolering_mjp_2027-2", 53.2336, 6.5750));
        assert!(check(&mut c, &f, "2026-11-03").unwrap().is_empty());
    }

    #[test]
    fn the_feed_is_atlas_s_unless_named() {
        let mut config = SimConfig::default();
        assert_eq!(feed_url(&config), None);
        config.atlas_url = Some("http://127.0.0.1:4749/".into());
        assert_eq!(
            feed_url(&config).as_deref(),
            Some("http://127.0.0.1:4749/plans/groningen.json")
        );
        config.plans_url = Some("http://172.17.0.1:4749/plans/groningen.json".into());
        assert_eq!(
            feed_url(&config).as_deref(),
            Some("http://172.17.0.1:4749/plans/groningen.json")
        );
    }
}
