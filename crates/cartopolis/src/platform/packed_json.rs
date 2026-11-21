//! Fetch a JSON cell file **packed first** (`cartopolis_geo::formats::packed`),
//! the JSON itself when the host has none.
//!
//! atlas writes `x.pj` beside every `x.json` cell whose coordinate pairs are
//! worth taking out of the text, about a third smaller after Brotli
//! (`docs/notes/cold-start-bytes.md`). Whatever arrives is handed back as the
//! JSON **text**, so a caller stores and parses exactly what it always did and
//! its cache keys do not move.
//!
//! One latch for the whole host, as `lod22` keeps for its binary cells: a host
//! that has served one packed file is never written off, and one that answers
//! 404 for a wave of them is read as JSON for the rest of the run. The latch is
//! per host rather than per layer because atlas writes every layer's sibling
//! from one place, so a host either has them all or has none.

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::time::Duration;

use cartopolis_geo::formats::packed;

use super::{http, workers};

const UNKNOWN: u8 = 0;
const YES: u8 = 1;
const NO: u8 = 2;

static SUPPORT: AtomicU8 = AtomicU8::new(UNKNOWN);
static FAILURES: AtomicU32 = AtomicU32::new(0);

/// Failed packed requests before the host is written off as not serving them:
/// about one opening wave of the layers' concurrent requests, so a single
/// missing cell among many in flight is not the decision.
const MAX_FAILURES: u32 = 8;

/// The JSON text at `json_url`, fetched packed when the host offers it.
///
/// # Errors
///
/// Whatever the JSON fetch itself answers; a failed packed attempt is never an
/// error, only a reason to ask for the JSON.
pub async fn fetch_text(json_url: String, timeout: Option<Duration>) -> Result<String, String> {
    if SUPPORT.load(Ordering::Relaxed) != NO {
        if let Some(packed_url) = packed::packed_path(&json_url) {
            let mut request = http::Request::get(packed_url);
            if let Some(timeout) = timeout {
                request = request.timeout(timeout);
            }
            match http::fetch_optional(request).await {
                Ok(Some(bytes)) if packed::looks_packed(&bytes) => {
                    // Unpacked off the frame: a dense cell is a megabyte or two
                    // of text to rebuild, and on the web this future runs on
                    // the thread that draws.
                    if let Some(text) =
                        workers::compute("packed json", move || packed::unpack(&bytes))
                            .await
                            .flatten()
                    {
                        SUPPORT.store(YES, Ordering::Relaxed);
                        return Ok(text);
                    }
                    note_failure();
                }
                // A 404, a body that is not packed (a proxy's error page), or a
                // transport failure: the JSON answers all three.
                _ => note_failure(),
            }
        }
    }
    let mut request = http::Request::get(json_url);
    if let Some(timeout) = timeout {
        request = request.timeout(timeout);
    }
    http::fetch_string(request).await
}

fn note_failure() {
    if SUPPORT.load(Ordering::Relaxed) == YES {
        return;
    }
    if FAILURES.fetch_add(1, Ordering::Relaxed) + 1 >= MAX_FAILURES {
        SUPPORT.store(NO, Ordering::Relaxed);
        tracing::info!("this host serves no packed JSON cells; reading the JSON");
    }
}
