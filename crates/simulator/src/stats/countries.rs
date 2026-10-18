//! Which country an address is in, for the web visitors' Countries list.
//!
//! The table is DB-IP's free "IP to Country Lite" (CC BY 4.0, which asks for the
//! attribution the Web visitors page carries): one CSV of address ranges, kept
//! beside the database as `dbip-country-lite.csv` and fetched again once a
//! month by [`keep_fresh`] when `geoip_download` is on. Looked up in memory at
//! the moment of the request, before the address goes to the visitor hash and is
//! dropped — the country is all that is kept.
//!
//! A CSV rather than DB-IP's MaxMind-format file because reading that needs a
//! crate this tree does not have; this is forty lines and a binary search over
//! about 700,000 ranges, some 15 MB.

use std::io::{BufRead, Read};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// The table in memory: ranges sorted by their start, each with its country.
#[derive(Default)]
pub struct Countries {
    v4: Vec<(u32, u32, [u8; 2])>,
    v6: Vec<(u128, u128, [u8; 2])>,
}

impl Countries {
    /// Read `start,end,country` lines. A line that does not parse is skipped.
    pub fn parse(reader: impl BufRead) -> Self {
        let mut c = Self::default();
        for line in reader.lines().map_while(Result::ok) {
            let mut f = line.split(',');
            let (Some(a), Some(b), Some(cc)) = (f.next(), f.next(), f.next()) else {
                continue;
            };
            let cc = cc.trim().trim_matches('"').as_bytes();
            if cc.len() != 2 || cc == b"ZZ" {
                continue;
            }
            let code = [cc[0], cc[1]];
            match (a.parse::<IpAddr>(), b.parse::<IpAddr>()) {
                (Ok(IpAddr::V4(a)), Ok(IpAddr::V4(b))) => c.v4.push((a.into(), b.into(), code)),
                (Ok(IpAddr::V6(a)), Ok(IpAddr::V6(b))) => c.v6.push((a.into(), b.into(), code)),
                _ => {}
            }
        }
        c.v4.sort_unstable_by_key(|r| r.0);
        c.v6.sort_unstable_by_key(|r| r.0);
        c
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let file = std::fs::File::open(path)?;
        Ok(Self::parse(std::io::BufReader::new(file)))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.v4.len() + self.v6.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The two-letter country of `ip`, or `None` when no range holds it.
    #[must_use]
    pub fn lookup(&self, ip: IpAddr) -> Option<String> {
        fn find<T: Ord + Copy>(v: &[(T, T, [u8; 2])], x: T) -> Option<String> {
            let i = v.partition_point(|r| r.0 <= x).checked_sub(1)?;
            let (_, end, cc) = v[i];
            (x <= end).then(|| String::from_utf8_lossy(&cc).into_owned())
        }
        match ip {
            IpAddr::V4(a) => find(&self.v4, u32::from(a)),
            IpAddr::V6(a) => match a.to_ipv4_mapped() {
                Some(v4) => find(&self.v4, u32::from(v4)),
                None => find(&self.v6, u128::from(a)),
            },
        }
    }
}

/// The year and month of a unix day (Howard Hinnant's `civil_from_days`).
#[must_use]
pub fn year_month(day: i64) -> (i64, u32) {
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, u32::try_from(m).unwrap_or(1))
}

const SOURCE: &str = "https://download.db-ip.com/free/dbip-country-lite";
/// How old the file may be before it is fetched again: DB-IP publishes monthly.
const MAX_AGE: Duration = Duration::from_secs(32 * 86_400);

/// Fetch this month's table (last month's while this month's is not out yet)
/// into `path`, through a file beside it so a half download is never read.
fn fetch(path: &Path) -> anyhow::Result<()> {
    let day = super::day_of(super::now_s());
    let (y, m) = year_month(day);
    let (py, pm) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
    let mut last = None;
    for (y, m) in [(y, m), (py, pm)] {
        let url = format!("{SOURCE}-{y}-{m:02}.csv.gz");
        match ureq::get(&url).timeout(Duration::from_secs(120)).call() {
            Ok(resp) => {
                let mut gz = Vec::new();
                resp.into_reader().take(64 << 20).read_to_end(&mut gz)?;
                let mut csv = Vec::new();
                flate2::read::GzDecoder::new(&gz[..]).read_to_end(&mut csv)?;
                if Countries::parse(&csv[..]).len() < 1000 {
                    anyhow::bail!("{url} held almost nothing");
                }
                let tmp = path.with_extension("csv.part");
                std::fs::write(&tmp, &csv)?;
                std::fs::rename(&tmp, path)?;
                tracing::info!(%url, "stats: country table fetched");
                return Ok(());
            }
            Err(e) => last = Some(anyhow::anyhow!("{url}: {e}")),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no country table")))
}

/// The table's file: beside the database.
#[must_use]
pub fn path_beside(database_path: &str) -> PathBuf {
    Path::new(database_path).with_file_name("dbip-country-lite.csv")
}

/// Load the table into `slot` now if it is on disk, and — when `download` is
/// on — fetch it when it is missing or a month old, checking once a day.
pub fn keep_fresh(slot: Arc<RwLock<Option<Arc<Countries>>>>, path: PathBuf, download: bool) {
    tokio::spawn(async move {
        loop {
            let p = path.clone();
            let stale = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .map_or(true, |t| t.elapsed().unwrap_or_default() > MAX_AGE);
            let loaded = slot.read().map(|s| s.is_some()).unwrap_or(false);
            if download && stale {
                let p = p.clone();
                match tokio::task::spawn_blocking(move || fetch(&p)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::warn!("stats: country table not fetched: {e:#}"),
                    Err(e) => tracing::warn!("stats: country table task: {e}"),
                }
            }
            if !loaded || (download && stale) {
                if let Ok(Ok(c)) = tokio::task::spawn_blocking(move || Countries::load(&p)).await {
                    tracing::info!(ranges = c.len(), "stats: country table loaded");
                    if let Ok(mut s) = slot.write() {
                        *s = Some(Arc::new(c));
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(86_400)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_answer_their_country() {
        let c = Countries::parse(
            &b"0.0.0.0,0.255.255.255,ZZ\n1.0.0.0,1.0.0.255,AU\n145.97.0.0,145.97.255.255,NL\n2001:610::,2001:610:ffff:ffff:ffff:ffff:ffff:ffff,NL\n"[..],
        );
        assert_eq!(
            c.lookup("145.97.1.2".parse().unwrap()).as_deref(),
            Some("NL")
        );
        assert_eq!(c.lookup("1.0.0.7".parse().unwrap()).as_deref(), Some("AU"));
        assert_eq!(c.lookup("1.0.1.0".parse().unwrap()), None);
        assert_eq!(c.lookup("0.1.2.3".parse().unwrap()), None);
        assert_eq!(
            c.lookup("2001:610::1".parse().unwrap()).as_deref(),
            Some("NL")
        );
        assert_eq!(
            c.lookup("::ffff:145.97.9.9".parse().unwrap()).as_deref(),
            Some("NL")
        );
    }

    #[test]
    fn months_from_days() {
        assert_eq!(year_month(0), (1970, 1));
        // 18 October 2026.
        assert_eq!(year_month(20_744), (2026, 10));
        assert_eq!(year_month(20_454), (2026, 1));
    }
}
