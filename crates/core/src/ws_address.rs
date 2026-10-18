//! What an app says about itself when it opens `/ws`: which app it is and the
//! commit it was built from, as query parameters the simulator's usage counts
//! read (`docs/notes/usage-stats.md`).
//!
//! On the address rather than in `ClientHello` because postcard is positional:
//! a field there is a protocol revision, and every installed app older than it
//! would be refused at sign-in. A server that does not look ignores the query.

/// `url` with `client` and, when known, `build` (shortened to 8 characters)
/// added to its query. A URL that already names a client is left alone.
#[must_use]
pub fn tagged(url: &str, client: &str, build: Option<&str>) -> String {
    if url.contains("client=") {
        return url.to_string();
    }
    let (base, fragment) = match url.split_once('#') {
        Some((b, f)) => (b, Some(f)),
        None => (url, None),
    };
    let sep = if base.contains('?') { '&' } else { '?' };
    let mut out = format!("{base}{sep}client={client}");
    if let Some(build) = build
        .map(|b| {
            b.chars()
                .filter(char::is_ascii_alphanumeric)
                .take(8)
                .collect::<String>()
        })
        .filter(|b| !b.is_empty())
    {
        out.push_str("&build=");
        out.push_str(&build);
    }
    if let Some(f) = fragment {
        out.push('#');
        out.push_str(f);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::tagged;

    #[test]
    fn adds_the_app_and_a_short_build() {
        assert_eq!(
            tagged("wss://cartopolis.org/ws", "android", Some("410d46f4abcdef")),
            "wss://cartopolis.org/ws?client=android&build=410d46f4"
        );
        assert_eq!(
            tagged("ws://127.0.0.1:8080/ws", "desktop", None),
            "ws://127.0.0.1:8080/ws?client=desktop"
        );
        assert_eq!(
            tagged("wss://h/ws?x=1", "web", Some("")),
            "wss://h/ws?x=1&client=web"
        );
        assert_eq!(
            tagged("wss://h/ws?client=chat", "web", None),
            "wss://h/ws?client=chat"
        );
    }
}
