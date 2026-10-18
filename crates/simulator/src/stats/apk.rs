//! APK downloads, counted from Caddy's access log.
//!
//! The APK is a file Caddy serves itself, with no page that could report it,
//! so Caddy is the one place that knows. Its `cartopolis.org` block logs only
//! requests for `/apk/cartopolis.apk`, strips the address and headers from each
//! line, and streams the lines to this listener over the docker gateway
//! (`output net`, `docs/notes/usage-stats.md`) — nothing is written to disk, and
//! a line is gone once counted. Only a whole download counts: a `GET` answered
//! `200`, not a resumed range (`206`) or a "not changed" (`304`).
//!
//! The listener accepts lines from anything that can reach it, so it binds the
//! docker gateway beside the HTTP listener, never a public address.

use super::{Event, Stats};
use std::sync::Arc;
use tokio::io::AsyncBufReadExt;

/// The path counted.
pub const APK_PATH: &str = "/apk/cartopolis.apk";

/// Whether one access-log line is a whole download of the APK.
#[must_use]
pub fn is_download(line: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    let req = &v["request"];
    let path = req["uri"]
        .as_str()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("");
    req["method"] == "GET" && path == APK_PATH && v["status"] == 200
}

/// Bind `addr` and count the downloads in every line Caddy sends, for the
/// life of the server.
pub async fn listen(
    addr: &str,
    stats: Arc<Stats>,
) -> anyhow::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                continue;
            };
            let stats = stats.clone();
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stream).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if line.len() <= 64 * 1024 && is_download(&line) {
                        stats.event(Event::ApkDownload, None, None);
                    }
                }
            });
        }
    });
    Ok((bound, task))
}

#[cfg(test)]
mod tests {
    use super::is_download;

    #[test]
    fn only_whole_downloads_count() {
        let line = |method: &str, uri: &str, status: u16| {
            format!(
                r#"{{"level":"info","msg":"handled request","request":{{"method":"{method}","host":"cartopolis.org","uri":"{uri}"}},"status":{status}}}"#
            )
        };
        assert!(is_download(&line("GET", "/apk/cartopolis.apk", 200)));
        assert!(is_download(&line("GET", "/apk/cartopolis.apk?v=2", 200)));
        assert!(!is_download(&line("GET", "/apk/cartopolis.apk", 206)));
        assert!(!is_download(&line("HEAD", "/apk/cartopolis.apk", 200)));
        assert!(!is_download(&line("GET", "/apk/", 200)));
        assert!(!is_download("not json"));
    }
}
