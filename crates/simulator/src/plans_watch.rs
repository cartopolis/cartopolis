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

use std::collections::BTreeSet;
use std::time::Duration;

use cartopolis_core::plans::{civil_date, near_followed, PlansFeed, NEAR_M};
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

/// Mark the feed's unchecked notices as checked, and answer the accounts that
/// follow a spot near one of them published on or after `since`
/// (`YYYY-MM-DD`). Nobody on a server's first read.
pub fn check(c: &mut Connection, feed: &PlansFeed, since: &str) -> anyhow::Result<Vec<String>> {
    let ids: Vec<&str> = feed.notices.iter().map(|n| n.id.as_str()).collect();
    let (unseen, checked_before) = db::unseen_notices(c, &ids)?;
    if unseen.is_empty() {
        return Ok(Vec::new());
    }
    db::mark_notices_seen(c, &unseen)?;
    if !checked_before {
        tracing::info!(
            notices = unseen.len(),
            "plans feed read for the first time; nobody woken for these"
        );
        return Ok(Vec::new());
    }
    let unseen: BTreeSet<&str> = unseen.iter().map(String::as_str).collect();
    let new: Vec<_> = feed
        .notices
        .iter()
        .filter(|n| unseen.contains(n.id.as_str()) && n.date.as_str() >= since)
        .collect();
    let accounts: BTreeSet<String> = db::plan_follows(c)?
        .into_iter()
        .filter(|(_, lat, lng)| new.iter().any(|n| near_followed([*lat, *lng], n)))
        .map(|(account, _, _)| account)
        .collect();
    tracing::debug!(new = new.len(), near_m = NEAR_M, "plans checked");
    Ok(accounts.into_iter().collect())
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
