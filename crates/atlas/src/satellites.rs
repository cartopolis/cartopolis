//! The satellite catalogue, mirrored: CelesTrak's element sets for every active
//! satellite, fetched every [`REFRESH`] and served to every client from here at
//! `/satellites/active.tle` (the client's `systems::satellites` draws them).
//!
//! **Why a mirror rather than each client asking CelesTrak.** CelesTrak refuses
//! an address that downloads the same group more often than about once every two
//! hours: measured 2026-10-08, the third full download from the VPS within an
//! hour got a `403`. A phone is its own address and is fine; the web build is
//! not, because a browser cannot read CelesTrak (it sends no CORS header) and so
//! every browser user would arrive through this server's one address. With the
//! mirror CelesTrak sees one polite client, and the map keeps its satellites
//! while CelesTrak is down — which it was, every page answering `500`, for
//! several minutes the same day (`docs/notes/satellites.md`).
//!
//! The last good copy is kept on disk and served until a newer one arrives; an
//! answer that is not element sets (an error page, a truncated body) never
//! replaces it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

/// Where the catalogue comes from: every active payload, as three-line element
/// sets. The text form is about half the size of the JSON one.
pub const DEFAULT_SOURCE: &str =
    "https://celestrak.org/NORAD/elements/gp.php?GROUP=active&FORMAT=tle";

/// How often the copy is refreshed: CelesTrak's own update interval, and the
/// rate it asks clients to keep to.
pub const REFRESH: Duration = Duration::from_secs(2 * 3600);

/// How long to wait after a failed refresh. Long on purpose: a refusal is
/// CelesTrak saying "too often", and asking again sooner only extends it.
pub const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(3600);

/// Fewer element sets than this is not the catalogue — CelesTrak's `active`
/// group held 15,944 on 2026-10-08 — and is not allowed to replace a good copy.
pub const MIN_SATELLITES: usize = 1000;

/// How long a client or a cache may keep an answer before asking again.
const CLIENT_MAX_AGE_S: u64 = 1800;

/// One copy of the catalogue, as served.
struct Copy {
    body: Arc<[u8]>,
    fetched: SystemTime,
}

/// The mirror: the copy in memory, the file it is kept in, and where it comes
/// from.
pub struct SatelliteMirror {
    file: PathBuf,
    source: String,
    current: RwLock<Option<Copy>>,
}

/// How many element sets a body holds, counting the records whose second and
/// third lines are element lines. Zero for anything that is not a TLE file.
#[must_use]
pub fn count_element_sets(body: &str) -> usize {
    let lines: Vec<&str> = body.lines().map(str::trim_end).collect();
    lines
        .windows(2)
        .filter(|w| w[0].starts_with("1 ") && w[1].starts_with("2 "))
        .count()
}

impl SatelliteMirror {
    /// A mirror keeping its copy under `data_dir/satellites/`, fetched from
    /// `source`. An empty `source` turns refreshing off: the mirror then serves
    /// whatever copy is on disk, which is what the tests and a box without
    /// outbound access want.
    #[must_use]
    pub fn new(data_dir: &Path, source: &str) -> Self {
        let file = data_dir.join("satellites").join("active.tle");
        let current = std::fs::read(&file).ok().and_then(|body| {
            let n = std::str::from_utf8(&body).map_or(0, count_element_sets);
            (n >= MIN_SATELLITES).then(|| Copy {
                fetched: std::fs::metadata(&file)
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH),
                body: body.into(),
            })
        });
        Self {
            file,
            source: source.to_string(),
            current: RwLock::new(current),
        }
    }

    /// How long until the copy is due a refresh (zero when there is none).
    fn due_in(&self) -> Duration {
        let fetched = self
            .current
            .read()
            .ok()
            .and_then(|c| c.as_ref().map(|c| c.fetched));
        fetched
            .and_then(|t| t.elapsed().ok())
            .map_or(Duration::ZERO, |age| REFRESH.saturating_sub(age))
    }

    /// Fetch once and, if the answer is a catalogue, keep it. Blocking.
    fn refresh_blocking(&self) -> Result<usize, String> {
        let agent = ureq::AgentBuilder::new()
            .user_agent("cartopolis-atlas (+https://cartopolis.org)")
            .timeout(Duration::from_secs(120))
            .build();
        let resp = agent.get(&self.source).call().map_err(|e| e.to_string())?;
        let mut body = String::new();
        std::io::Read::read_to_string(
            &mut std::io::Read::take(resp.into_reader(), 64 * 1024 * 1024),
            &mut body,
        )
        .map_err(|e| e.to_string())?;
        let n = count_element_sets(&body);
        if n < MIN_SATELLITES {
            return Err(format!(
                "answer holds {n} element sets, fewer than {MIN_SATELLITES}: not kept"
            ));
        }
        self.store(body.into_bytes())?;
        Ok(n)
    }

    /// Keep `body` as the current copy: on disk first (written beside and moved
    /// over, so a reader never sees half a file), then in memory.
    fn store(&self, body: Vec<u8>) -> Result<(), String> {
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = self.file.with_extension("tle.part");
        std::fs::write(&tmp, &body).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.file).map_err(|e| e.to_string())?;
        if let Ok(mut c) = self.current.write() {
            *c = Some(Copy {
                body: body.into(),
                fetched: SystemTime::now(),
            });
        }
        Ok(())
    }

    /// Refresh in the background for as long as the process runs.
    pub fn spawn_refresh(self: &Arc<Self>) {
        if self.source.is_empty() {
            return;
        }
        let mirror = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(mirror.due_in()).await;
                let m = mirror.clone();
                let result = tokio::task::spawn_blocking(move || m.refresh_blocking())
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                match result {
                    Ok(n) => tracing::info!(satellites = n, "satellite catalogue refreshed"),
                    Err(error) => {
                        tracing::warn!(%error, "satellite catalogue refresh failed; keeping the last copy");
                        tokio::time::sleep(RETRY_AFTER_FAILURE).await;
                    }
                }
            }
        });
    }
}

/// `GET /satellites/active.tle`: the current copy, or `503` before the first.
pub async fn serve(State(mirror): State<Arc<SatelliteMirror>>) -> Response {
    let copy = mirror
        .current
        .read()
        .ok()
        .and_then(|c| c.as_ref().map(|c| (c.body.clone(), c.fetched)));
    let Some((body, fetched)) = copy else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
            "the satellite catalogue has not been fetched yet",
        )
            .into_response();
    };
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "text/plain; charset=utf-8".to_string(),
            ),
            (
                header::CACHE_CONTROL,
                format!("public, max-age={CLIENT_MAX_AGE_S}"),
            ),
            (header::LAST_MODIFIED, httpdate(fetched)),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".to_string()),
        ],
        body.to_vec(),
    )
        .into_response()
}

/// An HTTP date (RFC 9110 IMF-fixdate) for `t`.
fn httpdate(t: SystemTime) -> String {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = secs / 86_400;
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    // Civil-from-days (Howard Hinnant's algorithm), for the proleptic Gregorian
    // calendar from 1970-01-01.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}, {day:02} {} {year} {h:02}:{m:02}:{s:02} GMT",
        WEEKDAYS[(days % 7) as usize],
        MONTHS[(month - 1) as usize]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = "ISS (ZARYA)
1 25544U 98067A   20194.88612269 -.00002218  00000-0 -31515-4 0  9992
2 25544  51.6461 221.2784 0001413  89.1723 280.4612 15.49507896236008
";

    #[test]
    fn element_sets_are_counted_and_an_error_page_is_none() {
        assert_eq!(count_element_sets(ONE), 1);
        assert_eq!(count_element_sets(&ONE.repeat(3)), 3);
        assert_eq!(count_element_sets("<html>403 Forbidden</html>"), 0);
    }

    #[test]
    fn a_short_answer_never_replaces_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let mirror = SatelliteMirror::new(dir.path(), "");
        // Below the floor: a copy on disk this small is not loaded either.
        mirror.store(ONE.as_bytes().to_vec()).unwrap();
        let reloaded = SatelliteMirror::new(dir.path(), "");
        assert!(reloaded.current.read().unwrap().is_none());
        // A full-sized one is, and is due no refresh for a while.
        mirror
            .store(ONE.repeat(MIN_SATELLITES).into_bytes())
            .unwrap();
        let reloaded = SatelliteMirror::new(dir.path(), "");
        assert!(reloaded.current.read().unwrap().is_some());
        assert!(reloaded.due_in() > REFRESH - Duration::from_secs(60));
    }

    #[test]
    fn dates_are_http_dates() {
        assert_eq!(
            httpdate(SystemTime::UNIX_EPOCH),
            "Thu, 01 Jan 1970 00:00:00 GMT"
        );
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_791_486_000);
        assert_eq!(httpdate(t), "Thu, 08 Oct 2026 19:00:00 GMT");
    }
}
