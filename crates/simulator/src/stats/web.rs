//! What one web page view is reduced to before it is counted: a site, a page
//! with its ids blanked, the site the visitor came from, a device class and a
//! language. Nothing else from the request is kept (`docs/mockups/stats/`).

use serde::Deserialize;

/// The sites that report page views. Anything else is refused, so a stranger
/// cannot invent rows.
pub const SITES: &[&str] = &["web", "viewer", "about"];

/// What a page sends to `POST /api/stats/hit`.
#[derive(Debug, Deserialize)]
pub struct HitBody {
    pub site: String,
    pub path: String,
    #[serde(default)]
    pub referrer: String,
    /// The window's width in CSS pixels; 0 when unknown.
    #[serde(default)]
    pub width: u32,
}

/// One page view, reduced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub site: &'static str,
    pub path: String,
    pub source: String,
    pub device: &'static str,
    pub lang: String,
}

/// `None` when the site is not one of ours.
#[must_use]
pub fn reduce(body: &HitBody, accept_language: &str) -> Option<Hit> {
    let site = SITES.iter().copied().find(|s| *s == body.site)?;
    Some(Hit {
        site,
        path: page(&body.path),
        source: source(&body.referrer),
        device: device(body.width),
        lang: language(accept_language),
    })
}

/// The page without its query, fragment or ids: `/building/0014100010938173`
/// and every other building is one row, `/building/:id`. A community's address
/// is a name and stays.
#[must_use]
pub fn page(path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or("");
    let mut out = String::new();
    for seg in path.split('/').filter(|s| !s.is_empty()).take(4) {
        out.push('/');
        if is_id(seg) {
            out.push_str(":id");
        } else {
            out.extend(seg.chars().take(32));
        }
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

fn is_id(seg: &str) -> bool {
    let digits = seg.chars().filter(char::is_ascii_digit).count();
    seg.len() > 32
        || (seg.len() >= 3 && digits == seg.len())
        || (seg.len() >= 16 && seg.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
        || digits >= 6
}

const SEARCH: &[&str] = &[
    "google.",
    "bing.",
    "duckduckgo.",
    "ecosia.",
    "startpage.",
    "qwant.",
    "yahoo.",
    "yandex.",
    "baidu.",
    "search.brave.",
    "kagi.",
];

/// The referring site's host, never its full address; search engines as one
/// row, so a search term cannot ride along in a host name.
#[must_use]
pub fn source(referrer: &str) -> String {
    let Some(host) = url::Url::parse(referrer)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
    else {
        return "direct".into();
    };
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    if SEARCH.iter().any(|s| host.contains(s)) {
        return "search engines".into();
    }
    host.chars().take(64).collect()
}

#[must_use]
pub fn device(width: u32) -> &'static str {
    match width {
        0 => "unknown",
        1..700 => "phone",
        700..1100 => "tablet",
        _ => "desktop",
    }
}

/// The first language the browser asks for, as its two- or three-letter code.
#[must_use]
pub fn language(accept_language: &str) -> String {
    let first = accept_language
        .split([',', ';', '-', '_'])
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if (2..=3).contains(&first.len()) && first.chars().all(|c| c.is_ascii_lowercase()) {
        first
    } else {
        "unknown".into()
    }
}

const BOTS: &[&str] = &[
    "bot",
    "crawl",
    "spider",
    "slurp",
    "headless",
    "lighthouse",
    "preview",
    "curl",
    "wget",
    "python",
    "go-http",
    "scrapy",
    "facebookexternalhit",
    "monitor",
    "http-client",
    "java/",
];

/// A request that is not a person reading the page: counted as "bots", nothing
/// more. Pages are counted from their own script, which most crawlers never
/// run; this catches the ones that do.
#[must_use]
pub fn is_bot(user_agent: &str) -> bool {
    let ua = user_agent.to_ascii_lowercase();
    ua.is_empty() || BOTS.iter().any(|b| ua.contains(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_lose_their_ids_and_queries() {
        assert_eq!(page("/building/0014100010938173?x=1"), "/building/:id");
        assert_eq!(
            page("/c/oosterpoort/street-party#top"),
            "/c/oosterpoort/street-party"
        );
        assert_eq!(page("/p/3f2a9c1e-7b1d-4c2a-9e3f-1a2b3c4d5e6f"), "/p/:id");
        assert_eq!(page(""), "/");
        assert_eq!(page("/a/b/c/d/e/f"), "/a/b/c/d");
    }

    #[test]
    fn sources_are_hosts_and_search_is_one_row() {
        assert_eq!(source(""), "direct");
        assert_eq!(
            source("https://www.google.nl/search?q=secret"),
            "search engines"
        );
        assert_eq!(source("https://mastodon.social/@x/1234"), "mastodon.social");
        assert_eq!(source("not a url"), "direct");
    }

    #[test]
    fn devices_languages_and_bots() {
        assert_eq!(device(390), "phone");
        assert_eq!(device(1440), "desktop");
        assert_eq!(language("nl-NL,nl;q=0.9,en;q=0.8"), "nl");
        assert_eq!(language("*"), "unknown");
        assert!(is_bot("Mozilla/5.0 (compatible; Googlebot/2.1)"));
        assert!(is_bot(""));
        assert!(!is_bot(
            "Mozilla/5.0 (Linux; Android 15) Chrome/131 Mobile Safari/537.36"
        ));
    }

    #[test]
    fn only_our_sites() {
        let body = |site: &str| HitBody {
            site: site.into(),
            path: "/".into(),
            referrer: String::new(),
            width: 0,
        };
        assert!(reduce(&body("web"), "").is_some());
        assert!(reduce(&body("evil"), "").is_none());
    }
}
