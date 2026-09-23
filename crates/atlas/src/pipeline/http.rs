//! Fetching from the open-data services, with the retry every extractor needs.
//!
//! Blocking (`ureq`), because a dataset runs on a blocking thread and pages one
//! request at a time on purpose: these are somebody else's servers under a run
//! that makes thousands of requests.

use std::io::Read;
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

fn fetch(url: &str, timeout: Duration) -> Result<Vec<u8>, String> {
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
    let mut last = String::new();
    for attempt in 0..attempts {
        ctx.check()?;
        match fetch(url, timeout)
            .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
        {
            Ok(v) => return Ok(v),
            Err(e) => {
                ctx.check()?;
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

/// A GET that is not JSON (a WFS capabilities document). Not retried.
pub fn get_text(ctx: &Ctx, url: &str, label: &str) -> Outcome<String> {
    ctx.check()?;
    let bytes = fetch(url, Duration::from_secs(120))
        .map_err(|e| PipelineError::Failed(format!("{label}: {e}")))?;
    String::from_utf8(bytes).map_err(|e| PipelineError::Failed(format!("{label}: {e}")))
}

/// A GET that may be a gzipped **file** — the CityJSON tiles are served as `.gz`
/// with no `Content-Encoding`, so the magic number is the test. Not retried.
pub fn get_maybe_gzipped_json(ctx: &Ctx, url: &str, label: &str) -> Outcome<Value> {
    ctx.check()?;
    let raw = fetch(url, Duration::from_secs(300))
        .map_err(|e| PipelineError::Failed(format!("{label}: {e}")))?;
    let bytes = if raw.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(raw.as_slice())
            .take(MAX_BODY)
            .read_to_end(&mut out)
            .map_err(|e| PipelineError::Failed(format!("{label}: gunzip: {e}")))?;
        out
    } else {
        raw
    };
    serde_json::from_slice(&bytes).map_err(|e| PipelineError::Failed(format!("{label}: {e}")))
}

/// A GET whose 404 is an answer rather than a failure: `Ok(None)` when the file
/// is not there, `Ok(Some(bytes))` when it is, an error for anything else. Not
/// retried — the callers treat any failure as "this piece is missing".
pub fn get_bytes_optional(ctx: &Ctx, url: &str, timeout: Duration) -> Outcome<Option<Vec<u8>>> {
    ctx.check()?;
    match agent().get(url).timeout(timeout).call() {
        Ok(response) => {
            let mut bytes = Vec::new();
            response
                .into_reader()
                .take(MAX_BODY)
                .read_to_end(&mut bytes)
                .map_err(|e| PipelineError::Failed(e.to_string()))?;
            Ok(Some(bytes))
        }
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(ureq::Error::Status(code, _)) => Err(PipelineError::Failed(format!("HTTP {code}"))),
        Err(e) => Err(PipelineError::Failed(e.to_string())),
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

    #[test]
    fn query_encodes_like_url_search_params() {
        assert_eq!(
            query(&[("bbox", "1,2,3,4"), ("filter", "a b"), ("t", "x&y")]),
            "bbox=1%2C2%2C3%2C4&filter=a+b&t=x%26y"
        );
    }
}
