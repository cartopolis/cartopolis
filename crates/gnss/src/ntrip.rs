//! NTRIP: RTCM corrections from a caster over HTTP-shaped TCP.
//!
//! An RTK rover is only centimetres with a base station's observations, and
//! NTRIP is how those arrive over the internet: a `GET /<mountpoint>` to a
//! caster, Basic auth, and an endless body of RTCM 3. The request is written
//! the way revision 1 writes it (`HTTP/1.0`, no chunking) because every caster
//! answers that, while a revision 2 request invites a chunked body that would
//! have to be undone before the bytes could go to the receiver.
//!
//! **The mountpoint may be left empty**, and then the nearest one is chosen:
//! the caster's sourcetable (`GET /`) lists every stream with the position of
//! its base, and the client picks the closest RTCM 3 stream to the receiver's
//! own first fix. Range matters — a single base is good for centimetres out to
//! some tens of kilometres and degrades with distance — so "nearest" is the
//! choice a person would make by hand, made without the hand.
//!
//! Network (VRS) mountpoints synthesise a base at the rover's position and need
//! that position: [`Session`] uploads the receiver's GGA every
//! [`GGA_INTERVAL`] when the settings allow it.
//!
//! **Over TLS when the caster asks for it.** Kadaster's public caster
//! (`ntrip-lab.kadaster.nl`, the national AGRS.NL and NETPOS stations) listens
//! on 443 behind an HTTPS gateway and nowhere else, so a plain-TCP client cannot
//! reach the Netherlands' own reference network at all. Port 443 therefore
//! always means TLS; any other port is plain unless [`Caster::tls`] says not.
//! rustls with Mozilla's roots compiled in, for the reason the client gives for
//! its own connection: Android has no system trust store a native library can
//! read, and native-tls would be OpenSSL.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

/// How often the rover's position is sent to the caster.
pub const GGA_INTERVAL: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest response head read before giving up on the caster.
const MAX_HEAD: usize = 16 * 1024;
/// Longest sourcetable read.
const MAX_SOURCETABLE: usize = 4 * 1024 * 1024;

/// Where to get corrections.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Caster {
    pub host: String,
    pub port: u16,
    /// Empty: choose the nearest from the sourcetable.
    pub mountpoint: String,
    pub username: String,
    pub password: String,
    /// Upload the rover's GGA (network mountpoints need it).
    pub send_position: bool,
    /// Connect over TLS. Implied by port 443 ([`Self::uses_tls`]).
    pub tls: bool,
}

impl Caster {
    /// Whether the connection is encrypted: asked for, or port 443, where
    /// nothing but TLS is ever listening.
    #[must_use]
    pub fn uses_tls(&self) -> bool {
        self.tls || self.port == 443
    }
}

/// The request line and headers for one path on the caster.
#[must_use]
pub fn request(caster: &Caster, path: &str) -> String {
    // The port is left out where it is the scheme's own: an HTTPS gateway in
    // front of a caster routes on the Host header and expects it bare.
    let host = if caster.port == 443 || caster.port == 80 {
        caster.host.clone()
    } else {
        format!("{}:{}", caster.host, caster.port)
    };
    let mut r = format!(
        "GET /{path} HTTP/1.0\r\nHost: {host}\r\nUser-Agent: NTRIP garage44-cartopolis/{}\r\nAccept: */*\r\n",
        env!("CARGO_PKG_VERSION"),
    );
    if !caster.username.is_empty() {
        let token = base64(format!("{}:{}", caster.username, caster.password).as_bytes());
        r.push_str(&format!("Authorization: Basic {token}\r\n"));
    }
    r.push_str("Connection: close\r\n\r\n");
    r
}

/// What the caster said to a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Data follows.
    Stream,
    /// A sourcetable follows — asked for, or what a revision-1 caster sends
    /// back for a mountpoint it does not have.
    SourceTable,
    Unauthorized,
    NotFound,
    Other(String),
}

/// Classify a response head (everything up to the blank line, or the status
/// line alone for `ICY 200 OK`).
#[must_use]
pub fn classify(head: &str) -> Answer {
    let status = head.lines().next().unwrap_or("").trim();
    if status.starts_with("ICY 200") {
        return Answer::Stream;
    }
    if status.starts_with("SOURCETABLE 200") {
        return Answer::SourceTable;
    }
    let code = status.split_whitespace().nth(1).unwrap_or("");
    if status.starts_with("HTTP/") {
        return match code {
            "200" => {
                let sourcetable = head.lines().any(|l| {
                    let l = l.to_ascii_lowercase();
                    l.starts_with("content-type:") && l.contains("gnss/sourcetable")
                });
                if sourcetable {
                    Answer::SourceTable
                } else {
                    Answer::Stream
                }
            }
            "401" | "403" => Answer::Unauthorized,
            "404" => Answer::NotFound,
            _ => Answer::Other(status.to_string()),
        };
    }
    Answer::Other(if status.is_empty() {
        "the caster closed the connection without answering".to_string()
    } else {
        status.chars().take(80).collect()
    })
}

/// One stream in a sourcetable (`STR;` line).
#[derive(Clone, Debug, PartialEq)]
pub struct Mount {
    pub name: String,
    /// The operator's own description, often the town.
    pub identifier: String,
    /// `RTCM 3.2`, `RTCM 3`, `CMR+`, …
    pub format: String,
    pub lat: f64,
    pub lng: f64,
    /// The stream wants the rover's GGA (network/VRS).
    pub wants_position: bool,
    /// The stream asks for a username and password (`B`asic or `D`igest).
    pub needs_login: bool,
}

/// The `STR` lines of a sourcetable.
#[must_use]
pub fn parse_sourcetable(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.trim().split(';').collect();
            if f.first() != Some(&"STR") || f.len() < 12 {
                return None;
            }
            Some(Mount {
                name: f[1].to_string(),
                identifier: f[2].to_string(),
                format: f[3].to_string(),
                lat: f[9].trim().parse().ok()?,
                lng: f[10].trim().parse().ok()?,
                wants_position: f[11].trim() == "1",
                needs_login: f.get(15).is_some_and(|a| matches!(a.trim(), "B" | "D")),
            })
        })
        .collect()
}

/// The nearest RTCM 3 stream to a position, with its distance in kilometres.
/// Without a login, only the streams that need none: Kadaster's table mixes
/// the open AGRS.NL stations with NETPOS ones that refuse an anonymous client,
/// and the nearest of all is often one of the latter.
#[must_use]
pub fn nearest(mounts: &[Mount], lat: f64, lng: f64, logged_in: bool) -> Option<(&Mount, f64)> {
    mounts
        .iter()
        .filter(|m| m.format.to_ascii_uppercase().starts_with("RTCM 3") || m.format == "RTCM3")
        .filter(|m| logged_in || !m.needs_login)
        // 0,0 is how a sourcetable says "no position given".
        .filter(|m| m.lat != 0.0 || m.lng != 0.0)
        .map(|m| (m, haversine_km(lat, lng, m.lat, m.lng)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
}

fn haversine_km(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lng2 - lng1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * 6371.0 * a.sqrt().asin()
}

/// RFC 4648 base64, for the one header that needs it.
fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A connection to a caster, plain or encrypted.
enum Conn {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Conn {
    fn socket(&self) -> &TcpStream {
        match self {
            Self::Plain(s) => s,
            Self::Tls(s) => s.get_ref(),
        }
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(buf),
            Self::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(buf),
            Self::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Tls(s) => s.flush(),
        }
    }
}

/// One TLS configuration for the process: Mozilla's roots, ring as the
/// provider. Named explicitly rather than taken from rustls's process default,
/// which nothing in this crate can be sure was installed.
fn tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
    static CONFIG: std::sync::OnceLock<Result<Arc<rustls::ClientConfig>, String>> =
        std::sync::OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .map(|b| Arc::new(b.with_root_certificates(roots).with_no_client_auth()))
            .map_err(|e| e.to_string())
        })
        .clone()
}

/// An open connection to a caster.
pub struct Session {
    stream: Conn,
    /// Bytes read past the head, which belong to the body.
    pending: Vec<u8>,
}

fn open(caster: &Caster, path: &str) -> Result<(Conn, String, Vec<u8>), String> {
    if caster.host.trim().is_empty() {
        return Err("no caster host set".into());
    }
    let addr = (caster.host.trim(), caster.port)
        .to_socket_addrs()
        .map_err(|e| format!("{}: {e}", caster.host))?
        .next()
        .ok_or_else(|| format!("{}: no address", caster.host))?;
    let socket = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .map_err(|e| format!("{}:{}: {e}", caster.host, caster.port))?;
    socket
        .set_read_timeout(Some(CONNECT_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let mut stream = if caster.uses_tls() {
        let name = rustls::pki_types::ServerName::try_from(caster.host.trim().to_string())
            .map_err(|e| format!("{}: {e}", caster.host))?;
        let tls = rustls::ClientConnection::new(tls_config()?, name)
            .map_err(|e| format!("{}: {e}", caster.host))?;
        Conn::Tls(Box::new(rustls::StreamOwned::new(tls, socket)))
    } else {
        Conn::Plain(socket)
    };
    stream
        .write_all(request(caster, path).as_bytes())
        .map_err(|e| e.to_string())?;
    let (head, rest) = read_head(&mut stream)?;
    Ok((stream, head, rest))
}

/// Read until the end of the response head. `ICY 200 OK` is followed directly
/// by the body on many revision-1 casters, with no headers and sometimes no
/// blank line, so it ends the head on its own.
fn read_head(stream: &mut Conn) -> Result<(String, Vec<u8>), String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok((String::from_utf8_lossy(&buf).into_owned(), Vec::new()));
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = find(&buf, b"\r\n\r\n") {
            let rest = buf.split_off(end + 4);
            return Ok((String::from_utf8_lossy(&buf).into_owned(), rest));
        }
        if buf.starts_with(b"ICY 200") {
            if let Some(end) = find(&buf, b"\r\n") {
                let mut rest = buf.split_off(end + 2);
                if rest.starts_with(b"\r\n") {
                    rest.drain(..2);
                }
                return Ok((String::from_utf8_lossy(&buf).into_owned(), rest));
            }
        }
        if buf.len() > MAX_HEAD {
            return Err("the caster's answer has no end".into());
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Fetch and parse the caster's sourcetable.
pub fn sourcetable(caster: &Caster) -> Result<Vec<Mount>, String> {
    let (mut stream, head, mut body) = open(caster, "")?;
    match classify(&head) {
        Answer::SourceTable | Answer::Stream => {}
        Answer::Unauthorized => return Err("the caster refused the username or password".into()),
        Answer::NotFound => return Err("the caster has no sourcetable".into()),
        Answer::Other(s) => return Err(s),
    }
    let mut chunk = [0u8; 8192];
    while body.len() < MAX_SOURCETABLE && !ends_table(&body) {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(e) if body.is_empty() => return Err(e.to_string()),
            Err(_) => break,
        }
    }
    // The head too: a caster that sends no blank line after its status line
    // has put the whole table there.
    Ok(parse_sourcetable(&format!(
        "{head}{}",
        String::from_utf8_lossy(&body)
    )))
}

fn ends_table(body: &[u8]) -> bool {
    body.len() >= 16 && find(&body[body.len() - 16..], b"ENDSOURCETABLE").is_some()
}

impl Session {
    /// Connect to one mountpoint.
    pub fn connect(caster: &Caster, mountpoint: &str) -> Result<Self, String> {
        let (stream, head, pending) = open(caster, mountpoint)?;
        match classify(&head) {
            Answer::Stream => {}
            Answer::SourceTable | Answer::NotFound => {
                return Err(format!("the caster has no mountpoint “{mountpoint}”"));
            }
            Answer::Unauthorized => {
                return Err("the caster refused the username or password".into());
            }
            Answer::Other(s) => return Err(s),
        }
        stream
            .socket()
            .set_read_timeout(Some(Duration::from_millis(500)))
            .map_err(|e| e.to_string())?;
        Ok(Self { stream, pending })
    }

    /// Read correction bytes. `Ok(0)` means nothing arrived within the read
    /// timeout; `Err` means the connection is gone.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        if !self.pending.is_empty() {
            let n = self.pending.len().min(buf.len());
            buf[..n].copy_from_slice(&self.pending[..n]);
            self.pending.drain(..n);
            return Ok(n);
        }
        match self.stream.read(buf) {
            Ok(0) => Err("the caster closed the connection".into()),
            Ok(n) => Ok(n),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(0)
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// Send the rover's GGA (a complete sentence, with its line end).
    pub fn send_gga(&mut self, sentence: &str) -> Result<(), String> {
        self.stream
            .write_all(sentence.as_bytes())
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(
            base64(b"centipede:centipede"),
            "Y2VudGlwZWRlOmNlbnRpcGVkZQ=="
        );
    }

    #[test]
    fn the_request_carries_auth_only_when_a_user_is_set() {
        let mut c = Caster {
            host: "caster.example".into(),
            port: 2101,
            ..Caster::default()
        };
        let r = request(&c, "GRON00NLD0");
        assert!(r.starts_with("GET /GRON00NLD0 HTTP/1.0\r\n"));
        assert!(!r.contains("Authorization"));
        assert!(r.ends_with("\r\n\r\n"));
        c.username = "u".into();
        c.password = "p".into();
        assert!(request(&c, "X").contains("Authorization: Basic dTpw\r\n"));
    }

    #[test]
    fn answers_are_classified_across_both_revisions() {
        assert_eq!(classify("ICY 200 OK\r\n"), Answer::Stream);
        assert_eq!(
            classify("SOURCETABLE 200 OK\r\nServer: x\r\n"),
            Answer::SourceTable
        );
        assert_eq!(
            classify("HTTP/1.1 200 OK\r\nContent-Type: gnss/sourcetable\r\n"),
            Answer::SourceTable
        );
        assert_eq!(
            classify("HTTP/1.1 200 OK\r\nContent-Type: gnss/data\r\n"),
            Answer::Stream
        );
        assert_eq!(
            classify("HTTP/1.0 401 Unauthorized\r\n"),
            Answer::Unauthorized
        );
        assert_eq!(classify("HTTP/1.1 404 Not Found\r\n"), Answer::NotFound);
        assert!(matches!(classify(""), Answer::Other(_)));
    }

    const TABLE: &str = "SOURCETABLE 200 OK\r\n\
        CAS;caster.example;2101;Example;Op;0;NLD;52.0;5.0;0.0.0.0;0;none\r\n\
        STR;GRON;Groningen;RTCM 3.2;1005(10),1077(1);2;GPS+GLO+GAL+BDS;NET;NLD;53.24;6.53;0;0;sNTRIP;none;B;N;3000;\r\n\
        STR;UTRE;Utrecht;RTCM 3.2;1005(10),1077(1);2;GPS;NET;NLD;52.09;5.12;0;0;sNTRIP;none;B;N;3000;\r\n\
        STR;VRS;Network;RTCM 3.1;1004;2;GPS;NET;NLD;0.00;0.00;1;1;sNTRIP;none;B;N;3000;\r\n\
        STR;CMRX;Old;CMR+;;2;GPS;NET;NLD;53.22;6.57;0;0;sNTRIP;none;B;N;3000;\r\n\
        ENDSOURCETABLE\r\n";

    #[test]
    fn the_nearest_rtcm3_stream_is_chosen() {
        let mounts = parse_sourcetable(TABLE);
        assert_eq!(mounts.len(), 4);
        assert!(mounts[2].wants_position);
        // Groningen's centre: the CMR+ stream is nearer but unusable.
        let (m, km) = nearest(&mounts, 53.2194, 6.5665, true).unwrap();
        assert_eq!(m.name, "GRON");
        assert!(km < 5.0, "{km}");
        // The VRS stream has no position and is never "nearest".
        let (m, _) = nearest(&mounts, 0.1, 0.1, true).unwrap();
        assert_ne!(m.name, "VRS");
    }

    /// Kadaster's table as it reads near Groningen (2026-10-13): the nearest
    /// stream is a NETPOS station that needs a login, the next an open AGRS.NL
    /// one. Without a login the open one is chosen.
    #[test]
    fn without_a_login_only_open_streams_are_chosen() {
        let table = "STR;DZYL00NLD0;Delfzijl;RTCM 3.3;;2;GPS+GLO+GAL+BDS+QZS;NETPOS;NLD;53.33;6.93;0;0;GNSMART;none;B;Y;0;\r\n\
            STR;DELZ00NLD0;Delfzijl Tide Gauge;RTCM 3.3;;2;GPS+GLO+GAL+BDS;AGRS.NL;NLD;53.34;6.93;0;0;GNSMART;none;N;N;0;\r\n";
        let mounts = parse_sourcetable(table);
        assert!(mounts[0].needs_login && !mounts[1].needs_login);
        assert_eq!(
            nearest(&mounts, 53.32, 6.93, false).unwrap().0.name,
            "DELZ00NLD0"
        );
        assert_eq!(
            nearest(&mounts, 53.32, 6.93, true).unwrap().0.name,
            "DZYL00NLD0"
        );
    }

    #[test]
    fn port_443_is_always_tls_and_sends_a_bare_host() {
        let c = Caster {
            host: "ntrip-lab.kadaster.nl".into(),
            port: 443,
            ..Caster::default()
        };
        assert!(c.uses_tls());
        assert!(request(&c, "X").contains("\r\nHost: ntrip-lab.kadaster.nl\r\n"));
    }
}
