//! Who may drive the pipeline: whoever the **simulator** says is an admin. Ported
//! from the earlier TypeScript server (`auth.ts`).
//!
//! Atlas mints nothing and stores no credential — yet. The web UI already holds
//! an admin bearer from the simulator's `/api/admin/login`, so the check is to hand
//! that token back to the simulator's `/api/admin/me` and believe the answer; that
//! route answers 200 only for an admin, so a 200 is the whole decision. A second
//! list of operators is how two lists come to disagree. When accounts move into
//! atlas (`docs/notes/atlas.md`) this is the one function that changes.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::{header, HeaderMap};
use serde::Deserialize;

/// How long an answer is believed. A web UI left open against an expired
/// session polls the job log every second; without this each poll would be a
/// round trip to the simulator. The staleness it buys is the staleness the
/// simulator already has: `disabled = 1` gates the next handshake, not a live
/// session.
const CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Principal {
    /// The simulator's account id. Kept opaque: atlas never keys on it.
    #[serde(default)]
    pub principal: serde_json::Value,
    pub username: String,
}

/// What the simulator says a session may do with one building
/// (`/api/buildings/{pand}/access`).
#[derive(Clone, Debug, Default, Deserialize, serde::Serialize, PartialEq)]
pub struct Access {
    pub username: String,
    #[serde(default)]
    pub admin: bool,
    #[serde(default)]
    pub owner: bool,
    #[serde(default)]
    pub can_edit: bool,
    /// Whose building it is, if anybody's.
    #[serde(default)]
    pub owned_by: Option<String>,
}

/// Something that can answer "is this bearer an admin?" and "may this bearer
/// change that building?", so a test need not run a simulator.
pub trait Authority: Send + Sync {
    /// `Ok(None)` is a refusal; `Err` is "could not ask", which is not cached.
    fn admin(&self, token: &str) -> Result<Option<Principal>, String>;

    /// `Ok(None)` is a dead session. Not cached: it is asked on opening the
    /// editor and on a save, and an ownership just given up must stop working
    /// at once.
    fn access(&self, _token: &str, _pand: &str) -> Result<Option<Access>, String> {
        Err("this authority does not know about buildings".into())
    }
}

/// The simulator's `/api/admin/me`.
pub struct Simulator {
    pub api: String,
}

impl Authority for Simulator {
    fn admin(&self, token: &str) -> Result<Option<Principal>, String> {
        let url = format!("{}/api/admin/me", self.api.trim_end_matches('/'));
        match ureq::get(&url)
            .set("authorization", &format!("Bearer {token}"))
            .timeout(Duration::from_secs(10))
            .call()
        {
            Ok(r) => serde_json::from_reader::<_, Principal>(r.into_reader())
                .map(Some)
                .map_err(|e| e.to_string()),
            Err(ureq::Error::Status(_, _)) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn access(&self, token: &str, pand: &str) -> Result<Option<Access>, String> {
        let url = format!(
            "{}/api/buildings/{pand}/access",
            self.api.trim_end_matches('/')
        );
        match ureq::get(&url)
            .set("authorization", &format!("Bearer {token}"))
            .timeout(Duration::from_secs(10))
            .call()
        {
            Ok(r) => serde_json::from_reader::<_, Access>(r.into_reader())
                .map(Some)
                .map_err(|e| e.to_string()),
            Err(ureq::Error::Status(_, _)) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}

pub struct Auth {
    authority: Box<dyn Authority>,
    cache: Mutex<HashMap<String, (Instant, Option<Principal>)>>,
}

/// The bearer token of a request, if it carries one.
#[must_use]
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    let token = headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?
        .trim();
    (!token.is_empty()).then_some(token)
}

impl Auth {
    /// What the bearer behind a request may do with a building. `None` for no
    /// session, a dead one, or a simulator that could not be asked — all of
    /// which a caller answers the same way: not you, not now.
    pub async fn access(
        self: &std::sync::Arc<Self>,
        headers: &HeaderMap,
        pand: &str,
    ) -> Option<Access> {
        let token = bearer(headers)?.to_string();
        let this = std::sync::Arc::clone(self);
        let pand = pand.to_string();
        match tokio::task::spawn_blocking(move || this.authority.access(&token, &pand))
            .await
            .ok()?
        {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!("building access check: {e}");
                None
            }
        }
    }

    pub fn new(authority: Box<dyn Authority>) -> Self {
        Self {
            authority,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The signed-in admin behind a request, or `None`.
    ///
    /// A refusal is cached as firmly as a grant. A simulator that cannot be
    /// reached is cached in neither direction: that is the absence of a decision,
    /// and caching it would turn a two-second blip into thirty seconds of
    /// locked-out operators. The request is still refused — again next time,
    /// rather than from memory.
    pub async fn admin(self: &std::sync::Arc<Self>, headers: &HeaderMap) -> Option<Principal> {
        let token = bearer(headers)?.to_string();
        {
            let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
            cache.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
            if let Some((_, who)) = cache.get(&token) {
                return who.clone();
            }
        }
        let this = std::sync::Arc::clone(self);
        let asked = token.clone();
        let answer = tokio::task::spawn_blocking(move || this.authority.admin(&asked))
            .await
            .ok()?;
        match answer {
            Ok(who) => {
                self.cache
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(token, (Instant::now(), who.clone()));
                who
            }
            Err(e) => {
                tracing::warn!("admin check: simulator unreachable: {e}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct Counting {
        calls: Arc<AtomicUsize>,
        reachable: bool,
    }

    impl Authority for Counting {
        fn admin(&self, token: &str) -> Result<Option<Principal>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if !self.reachable {
                return Err("connection refused".into());
            }
            Ok((token == "good").then(|| Principal {
                principal: serde_json::json!(1),
                username: "jeroen".into(),
            }))
        }
    }

    fn headers(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        h
    }

    #[tokio::test]
    async fn answers_are_cached_both_ways_but_an_outage_is_not() {
        let calls = Arc::new(AtomicUsize::new(0));
        let auth = Arc::new(Auth::new(Box::new(Counting {
            calls: Arc::clone(&calls),
            reachable: true,
        })));
        assert_eq!(
            auth.admin(&headers("good")).await.unwrap().username,
            "jeroen"
        );
        assert!(auth.admin(&headers("good")).await.is_some());
        assert!(auth.admin(&headers("bad")).await.is_none());
        assert!(auth.admin(&headers("bad")).await.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(auth.admin(&HeaderMap::new()).await.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let calls = Arc::new(AtomicUsize::new(0));
        let down = Arc::new(Auth::new(Box::new(Counting {
            calls: Arc::clone(&calls),
            reachable: false,
        })));
        assert!(down.admin(&headers("good")).await.is_none());
        assert!(down.admin(&headers("good")).await.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
