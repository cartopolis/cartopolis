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
pub const USER_AGENT: &str =
    "viberfox/0.2 (https://code.garage44.eu/garage44/viberfox; jvanveen@protonmail.com)";

/// Default per-request timeout when a caller does not set one.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// Hard cap on a response body, so a runaway or hostile upstream cannot exhaust
/// memory. 64 MiB comfortably covers the largest thing we fetch (a dense 3DBAG
/// LoD2.2 cell decompresses past 10 MB).
const MAX_BODY: u64 = 64 * 1024 * 1024;

/// Address a Public-tier upstream, direct natively and **same-origin** on the web.
///
/// A third party's server decides whether a browser may read its response, and a
/// plain `fetch` to one that says nothing fails outright — NOAA's NOMADS sends no
/// `Access-Control-Allow-Origin`, so the globe's cloud cover simply never arrived
/// on the web build. Our own services are not the problem (`tiles`, `overpass`,
/// `3dbag` and `nominatim` under `garage44.eu` all answer `*`); third parties are.
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

/// Fetch the response body as bytes. `Err` carries a human-readable message, to
/// match the `Result<_, String>` shape the fetchers already use.
///
/// `await` this inside a task spawned on `IoTaskPool` (see module docs); it is not
/// meant to be blocked on from a system.
pub async fn fetch(req: Request) -> Result<Vec<u8>, String> {
    imp::fetch(req).await
}

/// Fetch the response body and decode it as UTF-8.
pub async fn fetch_string(req: Request) -> Result<String, String> {
    let bytes = fetch(req).await?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

/// The shared native connection pool, for the rare caller that needs raw `ureq`
/// rather than [`fetch`] — e.g. one that destructures `ureq::Error::Status`, a
/// status code this module's `Result<_, String>` flattens away. Reaching for it
/// instead of `ureq::post` is what keeps such a caller on the one pool.
#[cfg(not(target_arch = "wasm32"))]
pub fn native_agent() -> &'static ureq::Agent {
    imp::agent()
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
    /// TCP + TLS handshake. Measured against `nominatim.garage44.eu` that was
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

    /// Hand out the shared agent — see [`super::native_agent`].
    pub(super) fn agent() -> &'static ureq::Agent {
        &AGENT
    }

    /// Native: a blocking `ureq` call. `fetch` is `async` for a uniform signature,
    /// but has no internal await points — it is expected to run on an `IoTaskPool`
    /// thread, where blocking is fine (that is what the pool is for). `ureq`'s
    /// `gzip` feature transparently inflates compressed responses.
    pub async fn fetch(req: Request) -> Result<Vec<u8>, String> {
        let mut builder = match req.method {
            Method::Get => AGENT.get(&req.url),
            Method::Post => AGENT.post(&req.url),
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

        let response = match req.body {
            Some(ref bytes) => builder.send_bytes(bytes),
            None => builder.call(),
        }
        .map_err(|e| e.to_string())?;

        let mut buf = Vec::new();
        response
            .into_reader()
            .take(MAX_BODY)
            .read_to_end(&mut buf)
            .map_err(|e| e.to_string())?;
        Ok(buf)
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::{Method, Request};

    /// Web: the browser `fetch`, via `ehttp`. Runs on the wasm executor; the
    /// `User-Agent` header (if any) is dropped by the browser. Identification-
    /// obligated upstreams are reached through the relay instead (ADR-034).
    pub async fn fetch(req: Request) -> Result<Vec<u8>, String> {
        let mut request = match req.method {
            Method::Get => ehttp::Request::get(&req.url),
            Method::Post => ehttp::Request::post(&req.url, req.body.clone().unwrap_or_default()),
        };
        for (name, value) in &req.headers {
            request.headers.insert(name, value);
        }
        let response = ehttp::fetch_async(request).await?;
        if !response.ok {
            return Err(format!("HTTP {} {}", response.status, response.status_text));
        }
        Ok(response.bytes)
    }
}
