//! Fetching from the open-data services, with the retry every extractor needs.
//!
//! Blocking (`ureq`), because a dataset runs on a blocking thread and pages one
//! request at a time on purpose: these are somebody else's servers under a run
//! that makes thousands of requests. Where a service offers one bulk download
//! instead of those thousands, prefer it (`bgt_extract.rs`).
//!
//! **Kept for a month** (`cached`). A re-run after a change to the pipeline's own
//! code wants the pipeline to run again, not the sources fetched again: the BGT
//! was hundreds of pages from PDOK — fifteen of the sixteen minutes of a 4 km
//! square, measured 2026-09-24, before it moved to one extract — and the 3D
//! basisvoorziening ~22 MB a 2 km sheet, both of which change on a scale of
//! weeks to a year. A run asked to be fresh (`Ctx::with_fresh`, `atlas run
//! --fresh`, the web UI's option) fetches everything anew and writes it back.
//! A request whose answer is a *state* rather than a source — a job's status —
//! must not go through it ([`get_json_live`]).

use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

use super::{Ctx, Outcome, PipelineError};

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .user_agent("cartopolis-atlas (+https://cartopolis.org)")
            .build()
    })
}

/// A response body is capped at this, so a misbehaving service cannot exhaust the
/// process. The largest legitimate one is a CityJSON tile, a few tens of MB.
const MAX_BODY: u64 = 512 * 1024 * 1024;

/// How long a fetched source answer is reused.
pub const SOURCE_CACHE_TTL: Duration = Duration::from_secs(30 * 24 * 3600);

/// A 64-bit FNV-1a of the URL: stable across builds, which `DefaultHasher` is
/// not promised to be, and a collision among one city's few thousand URLs is not
/// a real risk.
fn url_key(url: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in url.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

/// `fetch` through the source cache: a stored answer younger than
/// [`SOURCE_CACHE_TTL`] is reused unless the run is fresh, and a successful
/// fetch is stored. Failures are never stored — the next run asks again.
pub(super) fn cached(
    ctx: &Ctx,
    url: &str,
    fetch: impl FnOnce() -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    let Some((dir, read)) = ctx.source_cache() else {
        return fetch();
    };
    let key = url_key(url);
    let path = dir.join(&key[..2]).join(&key);
    if read {
        let young = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < SOURCE_CACHE_TTL);
        if young {
            if let Ok(bytes) = std::fs::read(&path) {
                return Ok(bytes);
            }
        }
    }
    let bytes = fetch()?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Through a temporary name, so a reader never sees half an answer.
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    if std::fs::write(&tmp, &bytes).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
    Ok(bytes)
}

/// Forget one cached answer — for a body that arrived with a 200 and would not
/// parse, which must not be served back for a month.
pub(super) fn evict(ctx: &Ctx, url: &str) {
    if let Some((dir, _)) = ctx.source_cache() {
        let key = url_key(url);
        let _ = std::fs::remove_file(dir.join(&key[..2]).join(&key));
    }
}

/// Delete every cached source answer older than [`SOURCE_CACHE_TTL`], and any
/// half-written one left by a run that died mid-write. Called at the start of
/// every run, so the cache holds at most a month of what runs actually fetched:
/// an area nobody re-runs ages out rather than staying for ever.
pub fn sweep_source_cache(data_dir: &std::path::Path) {
    let root = data_dir.join("source-cache");
    let Ok(buckets) = std::fs::read_dir(&root) else {
        return;
    };
    let (mut removed, mut bytes) = (0usize, 0u64);
    for bucket in buckets.flatten() {
        let Ok(entries) = std::fs::read_dir(bucket.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let old = meta
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_none_or(|age| age >= SOURCE_CACHE_TTL);
            let torn = path
                .extension()
                .is_some_and(|e| e.to_string_lossy().ends_with("tmp"))
                && meta
                    .modified()
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > Duration::from_secs(3600));
            if (old || torn) && std::fs::remove_file(&path).is_ok() {
                removed += 1;
                bytes += meta.len();
            }
        }
        // An emptied bucket goes too; `remove_dir` refuses one still in use.
        let _ = std::fs::remove_dir(bucket.path());
    }
    if removed > 0 {
        tracing::info!(removed, mb = bytes / (1024 * 1024), "source cache swept");
    }
}

pub(super) fn fetch(url: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    let response = agent()
        .get(url)
        .timeout(timeout)
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!("HTTP {code}"),
            other => other.to_string(),
        })?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_BODY)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// One JSON GET, retried: four attempts, three seconds apart, on *any* failure.
/// The failures that matter here are transient by nature — a dropped connection,
/// a 502 from a load balancer, a timeout under load — and a run that gives up on
/// the first one throws away twenty minutes of paging. A cancel is never retried
/// into the next attempt.
pub fn get_json(ctx: &Ctx, url: &str, label: &str) -> Outcome<Value> {
    get_json_with(ctx, url, label, Duration::from_secs(180), 4)
}

pub fn get_json_with(
    ctx: &Ctx,
    url: &str,
    label: &str,
    timeout: Duration,
    attempts: u32,
) -> Outcome<Value> {
    get_parsed_with(ctx, url, label, timeout, attempts, |b| {
        serde_json::from_slice(b).map_err(|e| e.to_string())
    })
}

/// A GET through the source cache, retried as [`get_json`] is, whose body is
/// turned into a `T` by `parse` — a body that will not parse is evicted and
/// fetched again, the way a truncated JSON answer is. For an image, where a
/// 200 carrying an XML service exception instead of a JPEG is the failure.
pub fn get_parsed_with<T>(
    ctx: &Ctx,
    url: &str,
    label: &str,
    timeout: Duration,
    attempts: u32,
    parse: impl Fn(&[u8]) -> Result<T, String>,
) -> Outcome<T> {
    let mut last = String::new();
    for attempt in 0..attempts {
        ctx.check()?;
        match cached(ctx, url, || fetch(url, timeout)).and_then(|b| parse(&b)) {
            Ok(v) => return Ok(v),
            Err(e) => {
                ctx.check()?;
                evict(ctx, url);
                last = e;
                if attempt + 1 == attempts {
                    break;
                }
                ctx.log(format!("{label}: {last} (retry {})", attempt + 1));
                ctx.sleep(Duration::from_secs(3))?;
            }
        }
    }
    Err(PipelineError::Failed(format!("{label}: {last}")))
}

/// A JSON GET that never goes through the source cache — for an answer that is
/// a state, like an extract's build status. Cached, the first "RUNNING" would be
/// served back to every later poll and the build would never be seen to finish.
/// Retried as [`get_json`] is.
pub fn get_json_live(ctx: &Ctx, url: &str, label: &str) -> Outcome<Value> {
    let mut last = String::new();
    for attempt in 0..4 {
        ctx.check()?;
        match fetch(url, Duration::from_secs(60))
            .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
        {
            Ok(v) => return Ok(v),
            Err(e) => {
                ctx.check()?;
                last = e;
                if attempt == 3 {
                    break;
                }
                ctx.log(format!("{label}: {last} (retry {})", attempt + 1));
                ctx.sleep(Duration::from_secs(3))?;
            }
        }
    }
    Err(PipelineError::Failed(format!("{label}: {last}")))
}

/// A JSON POST answered with JSON — asking PDOK to build an extract. Retried as
/// [`get_json`] is: a request that failed in transit built nothing, and one that
/// did build something only costs PDOK a second extract.
pub fn post_json(ctx: &Ctx, url: &str, body: &Value, label: &str) -> Outcome<Value> {
    let body = body.to_string();
    let mut last = String::new();
    for attempt in 0..4 {
        ctx.check()?;
        let answer = agent()
            .post(url)
            .timeout(Duration::from_secs(120))
            .set("content-type", "application/json")
            .send_string(&body)
            .map_err(|e| e.to_string())
            .and_then(|r| {
                serde_json::from_reader::<_, Value>(r.into_reader().take(MAX_BODY))
                    .map_err(|e| e.to_string())
            });
        match answer {
            Ok(v) => return Ok(v),
            Err(e) => {
                ctx.check()?;
                last = e;
                if attempt == 3 {
                    break;
                }
                ctx.log(format!("{label}: {last} (retry {})", attempt + 1));
                ctx.sleep(Duration::from_secs(3))?;
            }
        }
    }
    Err(PipelineError::Failed(format!("{label}: {last}")))
}

/// A GET streamed into a file rather than into memory, for an archive that can
/// run to gigabytes. Retried from the start. Answers the bytes written.
pub fn download_to(ctx: &Ctx, url: &str, path: &Path, label: &str) -> Outcome<u64> {
    let mut last = String::new();
    for attempt in 0..4 {
        ctx.check()?;
        let written = agent()
            .get(url)
            .timeout(Duration::from_secs(30 * 60))
            .call()
            .map_err(|e| e.to_string())
            .and_then(|r| {
                let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;
                std::io::copy(&mut r.into_reader(), &mut file).map_err(|e| e.to_string())
            });
        match written {
            Ok(n) => return Ok(n),
            Err(e) => {
                ctx.check()?;
                last = e;
                if attempt == 3 {
                    break;
                }
                ctx.log(format!("{label}: {last} (retry {})", attempt + 1));
                ctx.sleep(Duration::from_secs(3))?;
            }
        }
    }
    Err(PipelineError::Failed(format!("{label}: {last}")))
}

/// A GET that is not JSON (a WFS capabilities document). Not retried.
pub fn get_text(ctx: &Ctx, url: &str, label: &str) -> Outcome<String> {
    ctx.check()?;
    let bytes = cached(ctx, url, || fetch(url, Duration::from_secs(120)))
        .map_err(|e| PipelineError::Failed(format!("{label}: {e}")))?;
    String::from_utf8(bytes).map_err(|e| PipelineError::Failed(format!("{label}: {e}")))
}

/// A GET that may be a gzipped **file** — the CityJSON tiles are served as `.gz`
/// with no `Content-Encoding`, so the magic number is the test. Not retried.
pub fn get_maybe_gzipped_json(ctx: &Ctx, url: &str, label: &str) -> Outcome<Value> {
    ctx.check()?;
    let raw = cached(ctx, url, || fetch(url, Duration::from_secs(300)))
        .map_err(|e| PipelineError::Failed(format!("{label}: {e}")))?;
    let bytes = if raw.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(raw.as_slice())
            .take(MAX_BODY)
            .read_to_end(&mut out)
            .map_err(|e| {
                evict(ctx, url);
                PipelineError::Failed(format!("{label}: gunzip: {e}"))
            })?;
        out
    } else {
        raw
    };
    serde_json::from_slice(&bytes).map_err(|e| {
        evict(ctx, url);
        PipelineError::Failed(format!("{label}: {e}"))
    })
}

/// A GET whose 404 is an answer rather than a failure: `Ok(None)` when the file
/// is not there, `Ok(Some(bytes))` when it is, an error for anything else. Not
/// retried — the callers treat any failure as "this piece is missing".
pub fn get_bytes_optional(ctx: &Ctx, url: &str, timeout: Duration) -> Outcome<Option<Vec<u8>>> {
    ctx.check()?;
    // The 404 travels as an error the cache does not store, so "not there" is
    // asked again next run — it is cheap, and a sheet can be published since.
    const ABSENT: &str = "\u{0}absent";
    match cached(ctx, url, || match fetch(url, timeout) {
        Err(e) if e == "HTTP 404" => Err(ABSENT.to_string()),
        other => other,
    }) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e == ABSENT => Ok(None),
        Err(e) => Err(PipelineError::Failed(e)),
    }
}

/// Whether a `HEAD` answers 2xx — for probing which yearly edition exists.
pub fn head_ok(url: &str) -> bool {
    agent()
        .head(url)
        .timeout(Duration::from_secs(60))
        .call()
        .is_ok()
}

/// `a=1&b=2` in the order given, encoded as a browser's `URLSearchParams` does.
#[must_use]
pub fn query(params: &[(&str, &str)]) -> String {
    let mut s = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in params {
        s.append_pair(k, v);
    }
    s.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::Cell;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    struct Quiet;
    impl crate::pipeline::Sink for Quiet {
        fn log(&self, _: String) {}
        fn progress(&self, _: crate::pipeline::Progress) {}
    }

    fn ctx(data: &std::path::Path) -> Ctx {
        Ctx::new(
            [0.0; 4],
            "t",
            data,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Quiet),
        )
        .with_data_dir(data)
    }

    /// **A second run within the month asks nothing; a fresh one asks again and
    /// keeps what it got; a failure is never kept.**
    #[test]
    fn a_source_answer_is_kept_for_a_month_and_a_fresh_run_refetches() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Cell::new(0);
        let fetch = |body: &'static str| {
            calls.set(calls.get() + 1);
            Ok(body.as_bytes().to_vec())
        };
        let url = "https://api.pdok.nl/lv/bgt/ogc/v1/collections/wegdeel/items?page=1";
        let c = ctx(dir.path());
        assert_eq!(cached(&c, url, || fetch("one")).unwrap(), b"one");
        assert_eq!(cached(&c, url, || fetch("two")).unwrap(), b"one");
        assert_eq!(calls.get(), 1, "the second run asked again");

        let fresh = ctx(dir.path()).with_fresh(true);
        assert_eq!(cached(&fresh, url, || fetch("three")).unwrap(), b"three");
        assert_eq!(cached(&c, url, || fetch("four")).unwrap(), b"three");
        assert_eq!(calls.get(), 2);

        let other = "https://example.invalid/missing";
        assert!(cached(&c, other, || Err("HTTP 502".into())).is_err());
        assert_eq!(cached(&c, other, || fetch("five")).unwrap(), b"five");
    }

    /// A 200 whose body is not JSON is not kept: the retry asks again.
    #[test]
    fn a_body_that_does_not_parse_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let c = ctx(dir.path());
        let url = "https://a.invalid/page";
        cached(&c, url, || Ok(b"<html>busy</html>".to_vec())).unwrap();
        evict(&c, url);
        assert_eq!(cached(&c, url, || Ok(b"{}".to_vec())).unwrap(), b"{}");
    }

    /// **The next run clears what expired**, and a half-written answer a dead run
    /// left behind; what is still young stays.
    #[test]
    fn the_next_run_sweeps_what_expired() {
        let dir = tempfile::tempdir().unwrap();
        let c = ctx(dir.path());
        let (young, old) = ("https://a.invalid/young", "https://a.invalid/old");
        cached(&c, young, || Ok(b"y".to_vec())).unwrap();
        cached(&c, old, || Ok(b"o".to_vec())).unwrap();
        let root = dir.path().join("source-cache");
        let path_of = |url: &str| {
            let k = url_key(url);
            root.join(&k[..2]).join(k)
        };
        let past = std::time::SystemTime::now() - SOURCE_CACHE_TTL - Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(path_of(old))
            .unwrap()
            .set_modified(past)
            .unwrap();
        let torn = path_of(young).with_extension("123.tmp");
        std::fs::write(&torn, b"half").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&torn)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(7200))
            .unwrap();

        sweep_source_cache(dir.path());
        assert!(path_of(young).exists());
        assert!(!path_of(old).exists(), "the expired answer stayed");
        assert!(!torn.exists(), "the half-written answer stayed");
    }

    #[test]
    fn query_encodes_like_url_search_params() {
        assert_eq!(
            query(&[("bbox", "1,2,3,4"), ("filter", "a b"), ("t", "x&y")]),
            "bbox=1%2C2%2C3%2C4&filter=a+b&t=x%26y"
        );
    }
}
