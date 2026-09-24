//! HTTP provider (ADR-030/033) — one async fetch the whole client shares.
//!
//! Instead of each system spawning its own worker thread around a blocking
//! `ureq` call, callers build a [`Request`], `spawn` [`fetch`] on Bevy's
//! `IoTaskPool`, and drain the [`Task`](bevy::tasks::Task) from a system with
//! [`poll_task`]. The native impl runs blocking `ureq` (fine on a pool thread);
//! the wasm impl (behind `cfg(target_arch = "wasm32")`) uses the browser `fetch`
//! via `ehttp`. Neither the call sites nor this signature branch on platform.
//!
//! This is also where the nine divergent per-file `User-Agent` constants collapse
//! into one [`USER_AGENT`].

use std::time::Duration;

/// Descriptive, contact-bearing User-Agent. Several upstreams require one (MET,
/// OSM, Nominatim, Overpass, OVapi); this replaces the per-file constants.
///
/// A browser silently drops this header (`User-Agent` is forbidden in `fetch`),
/// so upstreams with a hard identification requirement are reached through the
/// relay on the web build instead (ADR-034), not directly.
#[cfg(not(target_arch = "wasm32"))]
pub const USER_AGENT: &str =
    "cartopolis/0.2 (https://code.garage44.eu/jeroen/cartopolis; jvanveen@protonmail.com)";

/// Default per-request timeout when a caller does not set one.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// Per-request bound for an **optional third-party layer** — one whose absence
/// costs its own decoration and nothing else, but whose fetch sits on the
/// critical path of something that matters.
///
/// The case this exists for is `systems::nav_marks` and `systems::crossings`:
/// both are awaited inside `map_geometry::spawn_geometry_task`, between the
/// cell's own tile body and the tessellation worker, so a slow `api.pdok.nl`
/// does not read as "no buoys today" — it reads as no ground, no roads and no
/// buildings, for as long as the request takes. On [`DEFAULT_TIMEOUT`] that is
/// 20 s per request, and the native fetch holds an `IoTaskPool` thread for all
/// of it (the native `CONNECT_TIMEOUT` caps only the connect phase).
///
/// **3 s**, from the measurement in #267: a cold start over Groningen made 18
/// requests to that service in 1.70 s — median 82 ms, worst 186 ms. Sixteen
/// times the observed worst case still delivers for a service that is merely
/// slow, while a dead one costs three seconds rather than twenty.
///
/// Native only, and knowingly: `ehttp` ignores `Request::timeout` in the browser
/// (see the wasm `imp`), which is a pre-existing gap shared with
/// `transit_vehicles`, `weather` and `globe_weather`. [`Breaker`] is what bounds
/// the damage there.
pub const OPTIONAL_LAYER_TIMEOUT: Duration = Duration::from_secs(3);

/// Hard cap on a response body, so a runaway or hostile upstream cannot exhaust
/// memory. 64 MiB comfortably covers the largest thing we fetch (a dense 3DBAG
/// LoD2.2 cell decompresses past 10 MB).
#[cfg(not(target_arch = "wasm32"))]
const MAX_BODY: u64 = 64 * 1024 * 1024;

/// Address a Public-tier upstream, direct natively and **same-origin** on the web.
///
/// A third party's server decides whether a browser may read its response, and a
/// plain `fetch` to one that says nothing fails outright — NOAA's NOMADS sends no
/// `Access-Control-Allow-Origin`, so the globe's cloud cover simply never arrived
/// on the web build. Our own services are not the problem (`tiles`, `overpass`,
/// `3dbag` and `nominatim` under `cartopolis.org` all answer `*`); third parties are.
///
/// Rather than ask each one for permission we stop being a third party to them:
/// the browser build requests `/up/<name>/…` on its **own** origin, where CORS
/// does not apply at all, and something server-side does the outward fetch. That
/// is ADR-034's position — the fix is one origin, not sprinkled CORS headers — and
/// it also puts identification back in reach, since a browser silently drops the
/// `User-Agent` that several upstreams require.
///
/// The server side is a deployment detail this function deliberately does not
/// know: `trunk serve` mirrors these paths with `[[proxy]]` entries in
/// `Trunk.toml`, and production path-mounts them in the same Caddy that already
/// fronts the rest. Native builds skip the hop entirely and go straight out.
///
/// `path` must start with `/` and may carry a query string.
#[must_use]
pub fn upstream_url(name: &str, origin: &str, path: &str) -> String {
    if cfg!(target_arch = "wasm32") {
        format!("/up/{name}{path}")
    } else {
        format!("{origin}{path}")
    }
}

/// HTTP method. Only the two the client uses.
// `Post` (and the `post`/`body` builders) are the provider API for the byte/POST
// fetchers still being migrated onto this seam; adopted incrementally per ADR-033.
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub enum Method {
    Get,
    Post,
    /// Undo an action a `Post` made — taking a model back out of the shared
    /// library, throwing an inventory item away.
    ///
    /// Here rather than modelled as a `Post` to a `/delete` path because the API
    /// on the other side is an ordinary REST surface that the atlas console also talks to,
    /// and inventing a second spelling for one of its two callers is how the two
    /// stop matching.
    Delete,
}

/// One HTTP request. Build with [`Request::get`] / [`Request::post`] and the
/// chained setters, e.g. `Request::get(url).header("Accept", "application/json")`.
#[derive(Clone)]
pub struct Request {
    pub url: String,
    pub method: Method,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub timeout: Duration,
}

#[allow(dead_code)] // `post`/`header`/`body`: provider API adopted as fetchers migrate.
impl Request {
    /// A GET for `url`, with the shared [`USER_AGENT`] and [`DEFAULT_TIMEOUT`]
    /// applied unless overridden.
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            method: Method::Get,
            headers: Vec::new(),
            body: None,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// A POST for `url`; set the payload with [`Request::body`].
    pub fn post(url: impl Into<String>) -> Self {
        Self {
            method: Method::Post,
            ..Self::get(url)
        }
    }

    /// A DELETE for `url`.
    pub fn delete(url: impl Into<String>) -> Self {
        Self {
            method: Method::Delete,
            ..Self::get(url)
        }
    }

    /// Add a request header. A `User-Agent` set here overrides [`USER_AGENT`].
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Override the request timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Attach a request body (implies POST semantics on the wire).
    #[must_use]
    pub fn body(mut self, body: Vec<u8>) -> Self {
        self.body = Some(body);
        self
    }
}

/// Cumulative accounting of every [`fetch`] the process has made, split into the
/// two halves that answer different questions: **time to first byte** (connect +
/// TLS + request + the server thinking — the half a protocol or pooling change
/// moves) and **body time** (bytes on the wire + inflate — the half only a
/// smaller payload moves). `docs/notes/tile-raster-cost.md` closes its HTTP/3
/// section with exactly this split being the unrecorded number that decides the
/// question; consumers diff two [`snapshot`](stats::snapshot)s, which is how the
/// shot harness turns it into per-capture columns.
///
/// On wasm the browser owns the connection and the split is invisible; only
/// `requests`, `bytes` and `errors` are counted there.
pub mod stats {
    use std::sync::atomic::{AtomicU64, Ordering};

    static REQUESTS: AtomicU64 = AtomicU64::new(0);
    static ERRORS: AtomicU64 = AtomicU64::new(0);
    static BYTES: AtomicU64 = AtomicU64::new(0);
    static TTFB_US: AtomicU64 = AtomicU64::new(0);
    static BODY_US: AtomicU64 = AtomicU64::new(0);

    /// One point-in-time reading of the counters.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct FetchTotals {
        /// Completed requests (errors count separately).
        pub requests: u64,
        pub errors: u64,
        /// Body bytes received, after transfer decompression.
        pub bytes: u64,
        /// Summed time to first byte, µs. Native only — 0 on wasm.
        pub ttfb_us: u64,
        /// Summed body read time, µs. Native only — 0 on wasm.
        pub body_us: u64,
    }

    impl FetchTotals {
        /// The delta `self − earlier`, for turning two snapshots into a window.
        #[must_use]
        pub fn since(&self, earlier: FetchTotals) -> FetchTotals {
            FetchTotals {
                requests: self.requests - earlier.requests,
                errors: self.errors - earlier.errors,
                bytes: self.bytes - earlier.bytes,
                ttfb_us: self.ttfb_us - earlier.ttfb_us,
                body_us: self.body_us - earlier.body_us,
            }
        }
    }

    #[must_use]
    pub fn snapshot() -> FetchTotals {
        FetchTotals {
            requests: REQUESTS.load(Ordering::Relaxed),
            errors: ERRORS.load(Ordering::Relaxed),
            bytes: BYTES.load(Ordering::Relaxed),
            ttfb_us: TTFB_US.load(Ordering::Relaxed),
            body_us: BODY_US.load(Ordering::Relaxed),
        }
    }

    pub(super) fn record(bytes: u64, ttfb_us: u64, body_us: u64) {
        REQUESTS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(bytes, Ordering::Relaxed);
        TTFB_US.fetch_add(ttfb_us, Ordering::Relaxed);
        BODY_US.fetch_add(body_us, Ordering::Relaxed);
    }

    pub(super) fn record_error() {
        ERRORS.fetch_add(1, Ordering::Relaxed);
    }

    /// The same account split by where the request went — host plus the first
    /// path segment, so `cartopolis.org/terrain` and `cartopolis.org/tiles` are
    /// two rows. The totals above say *how much* a load fetched; this says
    /// *what*, which is the question a warm start that still fetches answers.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Route {
        pub requests: u64,
        pub failed: u64,
        pub bytes: u64,
        /// Summed wall-clock per request, µs, on both platforms.
        pub us: u64,
    }

    static ROUTES: std::sync::Mutex<std::collections::BTreeMap<String, Route>> =
        std::sync::Mutex::new(std::collections::BTreeMap::new());

    /// `https://host/first/rest?q` → `host/first`.
    #[must_use]
    pub fn route_of(url: &str) -> String {
        let rest = url.split_once("://").map_or(url, |(_, r)| r);
        let rest = rest.split(['?', '#']).next().unwrap_or(rest);
        let mut parts = rest.split('/').filter(|s| !s.is_empty());
        match (parts.next(), parts.next()) {
            (Some(host), Some(first)) => format!("{host}/{first}"),
            (Some(host), None) => host.to_string(),
            _ => String::new(),
        }
    }

    pub(super) fn record_route(route: String, bytes: u64, us: u64, failed: bool) {
        let mut routes = match ROUTES.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let r = routes.entry(route).or_default();
        r.requests += 1;
        r.failed += u64::from(failed);
        r.bytes += bytes;
        r.us += us;
    }

    /// Every route seen so far, cumulative for the process.
    #[must_use]
    pub fn routes() -> Vec<(String, Route)> {
        let routes = match ROUTES.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        routes.iter().map(|(k, v)| (k.clone(), *v)).collect()
    }
}

/// How long a tripped [`Breaker`] stays open before the next request is allowed
/// through. 60 s, matching `tall_structures::RETRY_AFTER_SECS`, which is the
/// in-tree precedent for "a third party said no; come back later".
pub const BREAKER_COOLDOWN: Duration = Duration::from_secs(60);

/// A process-wide outage gate for one upstream service.
///
/// The problem it solves is not a slow request — [`OPTIONAL_LAYER_TIMEOUT`] is
/// that — but a *repeated* one. `map_geometry` rebuilds every cell in the ring
/// on a re-anchor, a terrain-field revision, a coverage revision or any
/// detail-layer toggle, and neither optional layer caches a failure (deliberately:
/// a poisoned cache key is never re-fetched, so half a cell written under one is
/// worse than a cell asked for again). Without a gate, each retear re-pays the
/// full wait for every cell.
///
/// So the failure is remembered per **service** rather than per cell: any fetch
/// or parse failure trips the breaker, and while it is open the layer's
/// `should_fetch` answers `false` and no request is issued at all. One instance
/// per module — RWS's marks service and ProRail's crossings service can fail
/// independently even though both sit behind `api.pdok.nl`.
///
/// The decision is a **pure predicate over an injected instant**
/// ([`Breaker::is_open_at`]), so a caller's gate can be asserted with no runtime
/// and no network — the same reason both layers split `should_fetch` out in the
/// first place. The clock is `web_time`: `std::time::Instant::now()` panics on
/// wasm, which is the one platform where this gate is the *only* bound (`ehttp`
/// ignores the request timeout there).
pub struct Breaker {
    /// When the breaker was last tripped, if it has been.
    tripped_at: std::sync::Mutex<Option<web_time::Instant>>,
}

impl Breaker {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            tripped_at: std::sync::Mutex::new(None),
        }
    }

    /// Record a failure. The breaker is open from `now` until
    /// [`BREAKER_COOLDOWN`] has passed.
    pub fn trip_at(&self, now: web_time::Instant) {
        if let Ok(mut slot) = self.tripped_at.lock() {
            *slot = Some(now);
        }
    }

    /// [`Breaker::trip_at`] with the current clock.
    pub fn trip(&self) {
        self.trip_at(web_time::Instant::now());
    }

    /// Is the service still being held off at `now`?
    #[must_use]
    pub fn is_open_at(&self, now: web_time::Instant) -> bool {
        self.tripped_at
            .lock()
            .ok()
            .and_then(|slot| *slot)
            // `checked_duration_since`, not `-`: `Instant` subtraction panics
            // when the argument is later, and an injected instant is free to be
            // one. `None` reads as closed, which costs one request.
            .and_then(|at| now.checked_duration_since(at))
            .is_some_and(|since| since < BREAKER_COOLDOWN)
    }
}

impl Default for Breaker {
    fn default() -> Self {
        Self::new()
    }
}

/// What came back, when the host was reached at all.
///
/// The distinction [`fetch`] flattens away and [`fetch_optional`] keeps: a host
/// that answers **404** was reached, understood the request and said the document
/// does not exist. That is an *answer*, and it will be the same answer tomorrow. A
/// connection that timed out is not an answer at all, and asking again is the only
/// way to find out. The two deserve opposite retry policies and had the same one.
pub enum Answer {
    Body(Vec<u8>),
    /// The host answered with a non-2xx status.
    Refused(u16),
}

/// The one call into the transport, with the per-route account kept around it
/// (`stats::routes`).
async fn answer(req: Request) -> Result<Answer, String> {
    let route = stats::route_of(&req.url);
    let started = bevy::platform::time::Instant::now();
    let result = imp::fetch_answer(req).await;
    let (bytes, failed) = match &result {
        Ok(Answer::Body(b)) => (b.len() as u64, false),
        Ok(Answer::Refused(_)) | Err(_) => (0, true),
    };
    stats::record_route(route, bytes, started.elapsed().as_micros() as u64, failed);
    result
}

/// Fetch the response body as bytes. `Err` carries a human-readable message, to
/// match the `Result<_, String>` shape the fetchers already use.
///
/// `await` this inside a task spawned on `IoTaskPool` (see module docs); it is not
/// meant to be blocked on from a system.
pub async fn fetch(req: Request) -> Result<Vec<u8>, String> {
    let url = req.url.clone();
    match answer(req).await? {
        Answer::Body(bytes) => Ok(bytes),
        // The shape `ureq` produced when this was one function, kept verbatim: it
        // is what every "… status code 404" line in the logs and the notes reads.
        Answer::Refused(status) => Err(format!("{url}: status code {status}")),
    }
}

/// Fetch a document the host is allowed not to have.
///
/// Three outcomes rather than two. `Ok(Some)` is the document; **`Ok(None)` is the
/// host saying it does not serve this**, which is a fact and not a failure; `Err` is
/// not having been able to ask.
///
/// The caller that needs this is any additive tree whose absence is permanent —
/// `systems::coverage`, where treating a 404 as transient cost three attempts thirty
/// seconds apart before a 67 ms answer was believed, and where the belief is what
/// retears a whole ring.
///
/// **Only 404 and 410 answer `None`.** A 5xx is the host failing rather than
/// answering, and a 403 may be a misconfiguration that is fixed while the client
/// runs; both keep the retry they had.
pub async fn fetch_optional(req: Request) -> Result<Option<Vec<u8>>, String> {
    let url = req.url.clone();
    match answer(req).await? {
        Answer::Body(bytes) => Ok(Some(bytes)),
        Answer::Refused(404 | 410) => Ok(None),
        Answer::Refused(status) => Err(format!("{url}: status code {status}")),
    }
}

/// Fetch the response body and decode it as UTF-8.
pub async fn fetch_string(req: Request) -> Result<String, String> {
    let bytes = fetch(req).await?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

/// Non-blocking poll of a spawned fetch task. Returns `Some(result)` on the frame
/// the task completes, `None` while it is still running. Call from a system that
/// holds the `Task`; it never blocks the schedule.
pub fn poll_task<T>(task: &mut bevy::tasks::Task<T>) -> Option<T> {
    futures_lite::future::block_on(futures_lite::future::poll_once(task))
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use super::{Method, Request, MAX_BODY, USER_AGENT};
    use std::io::Read;
    use std::sync::LazyLock;
    use std::time::Duration;

    /// Cap on the TCP connect phase alone, so an unreachable host fails fast
    /// instead of spending the caller's whole request timeout on a dead socket.
    /// That budget is shared: an io thread parked on a dead connect is one of at
    /// most eight (`main.rs`'s `TaskPoolOptions`) not streaming the map.
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

    /// One process-wide agent, so connections are **pooled and reused**.
    ///
    /// The free functions (`ureq::get`) build a fresh `Agent` — and therefore a
    /// fresh, empty connection pool — on every call, so each fetch paid a full
    /// TCP + TLS handshake. Measured against `nominatim.cartopolis.org` that was
    /// ~90 ms of a ~160 ms request: more than half the latency of a geocoding
    /// keystroke, spent re-negotiating a connection we had just closed. Keeping
    /// idle connections per host makes the second and later requests to the same
    /// endpoint start at the first byte.
    ///
    /// `max_idle_connections_per_host` defaults to 1, which would serialize the
    /// tile/building streamers onto a single socket; they fan out, so give them
    /// room. A connection only returns to the pool once its body is read to the
    /// end, which [`fetch`] always does.
    static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
        ureq::AgentBuilder::new()
            .max_idle_connections(64)
            .max_idle_connections_per_host(16)
            .timeout_connect(CONNECT_TIMEOUT)
            .build()
    });

    /// Native: a blocking `ureq` call. `fetch_answer` is `async` for a uniform
    /// signature, but has no internal await points — it is expected to run on an
    /// `IoTaskPool` thread, where blocking is fine (that is what the pool is for).
    /// `ureq`'s `gzip` feature transparently inflates compressed responses.
    pub async fn fetch_answer(req: Request) -> Result<super::Answer, String> {
        let mut builder = match req.method {
            Method::Get => AGENT.get(&req.url),
            Method::Post => AGENT.post(&req.url),
            Method::Delete => AGENT.delete(&req.url),
        };
        builder = builder.timeout(req.timeout);

        let mut has_user_agent = false;
        for (name, value) in &req.headers {
            if name.eq_ignore_ascii_case("user-agent") {
                has_user_agent = true;
            }
            builder = builder.set(name, value);
        }
        if !has_user_agent {
            builder = builder.set("User-Agent", USER_AGENT);
        }

        // Two stopwatches, split where the headers arrive: `call` returns once
        // the status line and headers are in (connect + TLS + request + server),
        // `read_to_end` is the body coming down and inflating. The split is what
        // `super::stats` exists to record — see its doc for who consumes it.
        let started = std::time::Instant::now();
        let response = match match req.body {
            Some(ref bytes) => builder.send_bytes(bytes),
            None => builder.call(),
        } {
            Ok(response) => response,
            // **A status is an answer.** `ureq` reports a non-2xx as an error, and
            // flattening it into the transport failures below is what made a
            // permanent 404 indistinguishable from a host that was merely down.
            Err(ureq::Error::Status(status, _)) => {
                super::stats::record_error();
                return Ok(super::Answer::Refused(status));
            }
            Err(e) => {
                super::stats::record_error();
                return Err(e.to_string());
            }
        };
        let ttfb = started.elapsed();

        let body_started = std::time::Instant::now();
        let mut buf = Vec::new();
        response
            .into_reader()
            .take(MAX_BODY)
            .read_to_end(&mut buf)
            .map_err(|e| {
                super::stats::record_error();
                e.to_string()
            })?;
        super::stats::record(
            buf.len() as u64,
            ttfb.as_micros() as u64,
            body_started.elapsed().as_micros() as u64,
        );
        Ok(super::Answer::Body(buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the optional-layer bound is that it is *shorter* than
    /// the one a fetcher inherits by not asking.
    #[test]
    fn the_optional_layer_bound_is_shorter_than_the_default() {
        assert!(OPTIONAL_LAYER_TIMEOUT < DEFAULT_TIMEOUT);
        assert_eq!(
            Request::get("http://example.invalid/").timeout,
            DEFAULT_TIMEOUT
        );
        assert_eq!(
            Request::get("http://example.invalid/")
                .timeout(OPTIONAL_LAYER_TIMEOUT)
                .timeout,
            OPTIONAL_LAYER_TIMEOUT
        );
    }

    /// The gate, as a pure function of an injected instant: no runtime, no
    /// network, no wall clock. A fresh breaker lets everything through; a
    /// tripped one holds the service off for exactly the cooldown and then
    /// reopens.
    #[test]
    fn a_tripped_breaker_reopens_after_the_cooldown() {
        let now = web_time::Instant::now();
        let breaker = Breaker::new();
        assert!(
            !breaker.is_open_at(now),
            "a fresh breaker holds nothing off"
        );

        breaker.trip_at(now);
        assert!(breaker.is_open_at(now));
        assert!(breaker.is_open_at(now + BREAKER_COOLDOWN - Duration::from_secs(1)));
        assert!(!breaker.is_open_at(now + BREAKER_COOLDOWN));
        assert!(!breaker.is_open_at(now + BREAKER_COOLDOWN + Duration::from_secs(1)));

        // …and a second failure inside the window restarts it, rather than
        // letting the first trip's deadline stand.
        let later = now + Duration::from_secs(30);
        breaker.trip_at(later);
        assert!(breaker.is_open_at(now + BREAKER_COOLDOWN));
        assert!(!breaker.is_open_at(later + BREAKER_COOLDOWN));
    }

    /// One instance per service: two upstreams behind one host fail
    /// independently, so tripping one must not hold the other off.
    #[test]
    fn breakers_are_independent_of_one_another() {
        let now = web_time::Instant::now();
        let one = Breaker::new();
        let other = Breaker::default();
        one.trip_at(now);
        assert!(one.is_open_at(now));
        assert!(!other.is_open_at(now));
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::{Method, Request};

    /// Web: the browser `fetch`, via `ehttp`. Runs on the wasm executor; the
    /// `User-Agent` header (if any) is dropped by the browser. Identification-
    /// obligated upstreams are reached through the relay instead (ADR-034).
    pub async fn fetch_answer(req: Request) -> Result<super::Answer, String> {
        let mut request = match req.method {
            Method::Get => ehttp::Request::get(&req.url),
            Method::Post => ehttp::Request::post(&req.url, req.body.clone().unwrap_or_default()),
            // `ehttp` has no constructor for it, so the method is set on a GET.
            Method::Delete => {
                let mut r = ehttp::Request::get(&req.url);
                r.method = "DELETE".into();
                r
            }
        };
        for (name, value) in &req.headers {
            request.headers.insert(name, value);
        }
        let response = match ehttp::fetch_async(request).await {
            Ok(response) => response,
            Err(e) => {
                super::stats::record_error();
                return Err(e);
            }
        };
        if !response.ok {
            super::stats::record_error();
            // The status, not a message: the caller decides whether this host has
            // answered a question or merely failed to. See `super::Answer`.
            return Ok(super::Answer::Refused(response.status));
        }
        // The browser owns the connection: no TTFB/body split is observable, and
        // `Instant` panics on wasm anyway. Count what is countable.
        super::stats::record(response.bytes.len() as u64, 0, 0);
        Ok(super::Answer::Body(response.bytes))
    }
}
