//! A link to a place — the URL that reproduces this exact view somewhere else.
//!
//! One string, two spellings of it:
//!
//! | | |
//! |---|---|
//! | shared | `https://cartopolis.org/go?place=53.21940,6.56650,120m,-12.5,45.0` |
//! | fallback | `cartopolis://go?place=53.21940,6.56650,120m,-12.5,45.0` |
//!
//! The https one is what gets shared: it is an **Android App Link** (verified
//! against `/.well-known/assetlinks.json`, see `docs/notes/place-links.md`), so on
//! a phone with the app installed the tap opens the app, and everywhere else it
//! opens the web client, which reads the same `?place=` it always has. The
//! `cartopolis:` one exists because verification is keyed to the *release* signing
//! certificate — a sideloaded local build gets no App Link at all — and because it
//! is what the web page's "Open in the app" button navigates to, a page having no
//! way to ask "is that app installed?" other than trying.
//!
//! **Why `/go` and not the site root.** An intent filter matches scheme, host and
//! path, and *never* the query — so `?place=` cannot be what distinguishes a link
//! that should open the app. Claiming the root instead would hand the app every
//! link to `cartopolis.org`, including the `/apk/` install page, which has to stay
//! in the browser or it is a page that installs the app you already have. `/go` is
//! the one path the app answers for. The root `?place=` URL keeps working in the
//! browser, so links shared before this existed still open something.
//!
//! The value is deliberately the **compact** spelling — no spaces, no degree signs
//! — so a link needs no percent-encoding and survives being pasted into a chat
//! client that ends a URL at the first space. [`place_from_link`] is liberal on the
//! way back in: that spelling, the readable `53.21940, 6.56650, 120m, -12.5°, 45.0°`
//! one a person types, either scheme, any host, and a bare coordinate string that is
//! not a URL at all.

use super::geo_nav::{parse_location, LocationFix};

/// The custom scheme, registered by `AndroidManifest.xml`.
pub const APP_SCHEME: &str = "cartopolis";

/// The origin the web client is published to (`deploy-wasm.yml`).
pub const WEB_ORIGIN: &str = "https://cartopolis.org";

/// The one path that opens the app. See the module note for why it is not `/`.
pub const LINK_PATH: &str = "go";

/// The query key, matching the `?place=` the web client already reads
/// (`crate::env_value` → `query_value("CARTO_PLACE")`).
pub const PLACE_KEY: &str = "place";

/// The place as a link carries it: `53.21940,6.56650,120m,-12.5,45.0`.
///
/// The same five numbers a person would write out as
/// `53.21940, 6.56650, 120m, -12.5°, 45.0°`, and the same parser reads both back —
/// but nothing in this one needs escaping. `both_spellings_mean_the_same_place`
/// below pins them to agree.
pub fn place_param(lat: f64, lng: f64, alt: f64, look: Option<(f32, f32)>) -> String {
    match look {
        Some((pitch, yaw)) => format!("{lat:.5},{lng:.5},{alt:.0}m,{pitch:.1},{yaw:.1}"),
        None => format!("{lat:.5},{lng:.5},{alt:.0}m"),
    }
}

/// The link to share: `https://cartopolis.org/go?place=…`.
pub fn web_link(lat: f64, lng: f64, alt: f64, look: Option<(f32, f32)>) -> String {
    format!(
        "{WEB_ORIGIN}/{LINK_PATH}?{PLACE_KEY}={}",
        place_param(lat, lng, alt, look)
    )
}

/// The query key a contact link carries (`systems::contacts`).
pub const CONTACT_KEY: &str = "contact";

/// A person's contact link: `https://cartopolis.org/go?contact=jeroen~3f9a1c7e22b0`.
///
/// On the `/go` path for the reason a place link is (it is the one path the
/// app claims on Android, and the web client serves it), with the name and the
/// code joined by `~`, which is not in any username and needs no escaping.
/// Whoever holds it may send a request whatever the person's setting says;
/// resetting the code in Contacts ▸ Add is how old copies stop working.
pub fn contact_link(username: &str, code: &str) -> String {
    format!("{WEB_ORIGIN}/{LINK_PATH}?{CONTACT_KEY}={username}~{code}")
}

/// The (username, code) a contact link carries, from either scheme and any
/// host (see [`link_place`] for why the host is not checked).
pub fn contact_from_link(input: &str) -> Option<(String, String)> {
    if !is_link(input.trim()) {
        return None;
    }
    let no_fragment = input.trim().split('#').next()?;
    let (_, query) = no_fragment.split_once('?')?;
    let value = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| key.trim().eq_ignore_ascii_case(CONTACT_KEY))
        .map(|(_, v)| percent_decode(v))?;
    let (user, code) = value.split_once('~')?;
    let (user, code) = (user.trim(), code.trim());
    (!user.is_empty() && !code.is_empty()).then(|| (user.to_string(), code.to_string()))
}

// There is deliberately no `app_link` beside `web_link`. The custom-scheme URL has
// exactly one writer and it is not Rust: `index.html`'s loader builds
// `cartopolis://go?place=…` in JavaScript, before any of this exists, because the
// page has to offer it *while the module is still downloading*. A Rust function
// nothing called would be a second definition of that string, drifting quietly.
// What keeps the two in step is that this module only ever *reads* the scheme, and
// reads it liberally — `a_link_round_trips_through_both_schemes` parses the exact
// literal the loader writes.

/// Turn `https://cartopolis.org/go?place=…` — or the `cartopolis:` spelling, or a
/// bare `53.2194, 6.5665, 120m` — into the view it names.
///
/// Deliberately liberal about where the value came from. A link arrives from an
/// Android intent, from `CARTO_PLACE`, from `?place=` and from whatever the user
/// pasted into the search bar, and the ones this refuses are the ones nobody can
/// fix: a URL with no place in it, and a place that does not parse.
pub fn place_from_link(input: &str) -> Option<LocationFix> {
    let raw = input.trim();
    match link_place(raw) {
        Some(value) => parse_location(&value),
        // Not a link at all — the pasted-coordinates path this has always had.
        None => parse_location(raw),
    }
}

/// Whether `input` looks like a URL rather than a coordinate string.
///
/// The scheme test is what keeps `place_from_link` from mangling a plain place:
/// `53.2194, 6.5665` has no `://` and no `cartopolis:`, so it never reaches the
/// query splitter.
fn is_link(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    lower.starts_with(&format!("{APP_SCHEME}:")) || lower.contains("://")
}

/// The `place` a link carries, or its last path segment if it carries no query.
///
/// The host is not checked. A link is worth following from `trunk serve` on
/// `localhost:8080` and from a mirror of the site as much as from the published
/// origin, and refusing those would buy nothing: the value is still run through
/// [`parse_location`], which is the check that matters.
fn link_place(input: &str) -> Option<String> {
    if !is_link(input) {
        return None;
    }
    // Strip the fragment first: `#` ends the query, and a link pasted out of a
    // browser may carry one that is nothing to do with us.
    let no_fragment = input.split('#').next().unwrap_or(input);
    if let Some((path, query)) = no_fragment.split_once('?') {
        let found = query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(key, _)| {
                let key = key.trim().to_ascii_lowercase();
                key == PLACE_KEY || key == format!("carto_{PLACE_KEY}")
            })
            .map(|(_, value)| percent_decode(value));
        if found.is_some() {
            return found;
        }
        // A query with no `place` in it (`?log`, a tracking parameter someone's
        // chat client bolted on) still leaves the path to try.
        return last_segment(path);
    }
    last_segment(no_fragment)
}

/// The last non-empty path segment, so `cartopolis://go/53.2194,6.5665` works too.
///
/// Not the form anything here *writes*, but it is the shape people type, and a
/// link that dropped its query somewhere still names a place in its path.
///
/// Worth knowing that this is a **custom-scheme** courtesy and not a supported
/// https URL: the site serves the client at exactly `/go`, because `index.html`
/// fetches Bevy's asset root relatively and a directory URL would resolve every
/// `assets/…` to `/go/assets/…`. So `https://cartopolis.org/go/53.2194,6.5665`
/// parses here and 404s in a browser — see `docs/notes/place-links.md`.
fn last_segment(url: &str) -> Option<String> {
    let path = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = path.strip_prefix("//").unwrap_or(path);
    let segment = path.rsplit('/').find(|s| !s.trim().is_empty())?;
    let decoded = percent_decode(segment);
    (!decoded.trim().is_empty()).then_some(decoded)
}

/// Percent-decode a query value, `+` included.
///
/// Written here rather than pulled in: the encoder this tree already has
/// (`geo_nav::percent_encode`) has no counterpart, and the alternative is a
/// dependency for twenty lines. An invalid escape is left alone rather than
/// dropped — `%` is not a character any place string contains, so anything that
/// reaches this having one is already something [`parse_location`] will refuse,
/// and refusing it *there* is the error message that names the actual problem.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_contact_link_reads_back_as_the_name_and_code_it_was_made_from() {
        let link = contact_link("sanne_k", "3f9a1c7e22b0");
        assert_eq!(
            contact_from_link(&link),
            Some(("sanne_k".into(), "3f9a1c7e22b0".into()))
        );
        assert_eq!(
            contact_from_link("cartopolis://go?contact=eva~abc#x"),
            Some(("eva".into(), "abc".into()))
        );
        // A place link is not a contact link, and a bare name is not a link.
        assert_eq!(contact_from_link(&web_link(53.2, 6.5, 100.0, None)), None);
        assert_eq!(contact_from_link("eva~abc"), None);
    }

    fn fix(input: &str) -> LocationFix {
        place_from_link(input).unwrap_or_else(|| panic!("{input} should name a place"))
    }

    fn assert_groningen(f: &LocationFix) {
        assert!((f.lat - 53.2194).abs() < 1e-5, "lat {}", f.lat);
        assert!((f.lng - 6.5665).abs() < 1e-5, "lng {}", f.lng);
        assert_eq!(f.altitude, Some(120.0));
        assert_eq!(f.look, Some((-12.5, 45.0)));
    }

    /// The contract the whole feature rests on: what **Copy link** writes is what
    /// a launch reads back.
    ///
    /// The `cartopolis:` literal here is not decoration — it is the exact string
    /// `index.html`'s loader builds in JavaScript, and this is the only place the
    /// two are held together (see the note above `web_link`).
    #[test]
    fn a_link_round_trips_through_both_schemes() {
        let web = web_link(53.2194, 6.5665, 120.0, Some((-12.5, 45.0)));
        assert_eq!(
            web,
            "https://cartopolis.org/go?place=53.21940,6.56650,120m,-12.5,45.0"
        );
        assert_groningen(&fix(&web));

        assert_groningen(&fix(
            "cartopolis://go?place=53.21940,6.56650,120m,-12.5,45.0",
        ));

        // The angles are written at one decimal, so a whole-degree yaw carries its
        // `.0`. Nothing depends on that — a hand-typed `45` is the same view — but
        // the assertions above are on *generated* strings, and the day someone
        // trims the zero they should be told which two places to look at.
        assert_groningen(&fix("cartopolis://go?place=53.21940,6.56650,120m,-12.5,45"));
    }

    /// The URL spelling and the readable one people type are two spellings of a
    /// single place, not two formats that happen to look alike — if they ever
    /// disagree, a link and a pasted string send you to different views.
    #[test]
    fn both_spellings_mean_the_same_place() {
        let readable = "53.21940, 6.56650, 120m, -12.5°, 45.0°";
        let compact = place_param(53.2194, 6.5665, 120.0, Some((-12.5, 45.0)));
        assert_eq!(compact, "53.21940,6.56650,120m,-12.5,45.0");
        assert_eq!(parse_location(readable), parse_location(&compact));

        // A place with no camera in it keeps the shorter tail.
        let no_look = place_param(-33.9249, -18.4241, 1500.0, None);
        assert_eq!(no_look, "-33.92490,-18.42410,1500m");
        let f = fix(&format!("{APP_SCHEME}://{LINK_PATH}?{PLACE_KEY}={no_look}"));
        assert_eq!(f.look, None);
    }

    /// Links shared before `/go` existed point at the site root, and a link that
    /// stops working is worse than a link that was never made.
    #[test]
    fn the_old_root_url_still_names_a_place() {
        assert_groningen(&fix(
            "https://cartopolis.org/?place=53.21940,6.56650,120m,-12.5,45",
        ));
        // …as does a dev server, which is a different host and port entirely.
        assert_groningen(&fix(
            "http://localhost:8080/go?place=53.21940,6.56650,120m,-12.5,45",
        ));
    }

    /// A bare coordinate string is not a URL and must not be treated as one — this
    /// is the search bar's existing paste path, which runs through here now.
    #[test]
    fn a_pasted_place_is_still_a_place() {
        assert_groningen(&fix("53.21940, 6.56650, 120m, -12.5°, 45.0°"));
        assert_eq!(place_from_link("Groningen"), None);
        assert_eq!(place_from_link(""), None);
    }

    /// Decoration a link picks up in transit: a fragment, an unrelated query
    /// parameter, an encoded comma from a client that escaped the value anyway.
    #[test]
    fn a_link_survives_being_passed_around() {
        assert_groningen(&fix(
            "https://cartopolis.org/go?place=53.21940,6.56650,120m,-12.5,45#map",
        ));
        assert_groningen(&fix(
            "https://cartopolis.org/go?log&place=53.21940,6.56650,120m,-12.5,45",
        ));
        assert_groningen(&fix(
            "https://cartopolis.org/go?place=53.21940%2C6.56650%2C120m%2C-12.5%2C45",
        ));
        // The percent-encoded readable form, which is what a naive encoder makes
        // of a copied string: spaces as `+` or `%20`, degree signs as UTF-8.
        assert_groningen(&fix(
            "cartopolis://go?place=53.21940,+6.56650,+120m,+-12.5%C2%B0,+45.0%C2%B0",
        ));
        // Path-carried, which nothing writes but people type.
        assert_groningen(&fix("cartopolis://go/53.21940,6.56650,120m,-12.5,45"));
    }

    /// A URL with nothing usable in it is refused rather than guessed at.
    #[test]
    fn a_link_to_nothing_is_refused() {
        assert_eq!(place_from_link("https://cartopolis.org/"), None);
        assert_eq!(place_from_link("https://cartopolis.org/apk/"), None);
        assert_eq!(place_from_link("cartopolis://go?place=Groningen"), None);
        assert_eq!(place_from_link("https://example.com/go?place=300,4"), None);
    }
}
