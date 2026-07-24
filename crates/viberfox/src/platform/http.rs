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

    /// Native: a blocking `ureq` call. `fetch` is `async` for a uniform signature,
    /// but has no internal await points — it is expected to run on an `IoTaskPool`
    /// thread, where blocking is fine (that is what the pool is for). `ureq`'s
    /// `gzip` feature transparently inflates compressed responses.
    pub async fn fetch(req: Request) -> Result<Vec<u8>, String> {
        let mut builder = match req.method {
            Method::Get => ureq::get(&req.url),
            Method::Post => ureq::post(&req.url),
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
