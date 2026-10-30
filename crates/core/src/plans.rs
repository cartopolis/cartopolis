//! What is planned where: a municipality's official notices, placed where each
//! notice says it is, and the council business that names the same places
//! (`docs/notes/plans.md`, the design is `docs/mockups/plans-and-decisions/`).
//!
//! These are the JSON shapes of atlas's `/plans/{municipality}.json`, which
//! atlas builds from three open sources: the notices from KOOP's repository of
//! official publications (each carries its own point, line or area), the
//! council's meetings from its own site, and street and neighbourhood names
//! from PDOK's location server. The client's Plans layer
//! (`systems::plans`) reads them; nothing here is on the wire.
//!
//! **A notice's place is published; a council item's is not.** A notice is
//! drawn at the coordinates the municipality put on it. A council agenda item
//! has none: it is placed by the street and square names in its title, which
//! is a guess, and every place it is given carries [`CouncilPlace::name`] so
//! the reader can see which word it was matched on.

use serde::{Deserialize, Serialize};

/// What sort of notice it is, as far as the layer's filters care.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    /// Somebody asked for a permit and the municipality has not decided yet.
    Application,
    /// A permit, or another decision on one case, granted or refused.
    Decision,
    /// A notification the municipality only takes note of: demolition, asbestos
    /// removal, an environmental activity.
    Notification,
    /// An event or market licence.
    Event,
    /// A traffic order.
    Traffic,
    /// Everything else with a place: camera zones, designations, ecology plans.
    Other,
}

impl NoticeKind {
    /// Every kind, in the order the layer's filters list them.
    pub const ALL: [Self; 6] = [
        Self::Application,
        Self::Decision,
        Self::Notification,
        Self::Event,
        Self::Traffic,
        Self::Other,
    ];

    /// The key a kind is saved under, the serde name.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Application => "application",
            Self::Decision => "decision",
            Self::Notification => "notification",
            Self::Event => "event",
            Self::Traffic => "traffic",
            Self::Other => "other",
        }
    }

    /// Classify a notice from its KOOP type (`dcterms:type`) and its title.
    ///
    /// The type alone is not enough: an application, a granted permit, an
    /// extension of the deciding period and a demolition notification are all
    /// `omgevingsvergunning`, and only the title's opening words tell them
    /// apart (measured on Groningen's notices of August–October 2026).
    #[must_use]
    pub fn classify(koop_type: &str, title: &str) -> Self {
        let t = title.to_lowercase();
        match koop_type {
            "evenementenvergunning" => return Self::Event,
            "verkeersbesluit of -mededeling" => return Self::Traffic,
            _ => {}
        }
        if t.contains("melding") {
            return Self::Notification;
        }
        if t.contains("aanvraag") || t.contains("ontwerpbesluit") {
            // "verlenging beslistermijn aanvraag" is still an open application.
            return Self::Application;
        }
        if t.contains("besluit")
            || t.contains("verleend")
            || t.contains("geweigerd")
            || t.contains("vergunning")
            || t.contains("toestemming")
        {
            return Self::Decision;
        }
        if matches!(
            koop_type,
            "omgevingsvergunning" | "andere vergunning" | "andere beschikking"
        ) {
            return Self::Decision;
        }
        Self::Other
    }
}

/// One official notice, where it says it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notice {
    /// KOOP's identifier, `gmb-2026-501719`. The official text is at
    /// [`notice_url`].
    pub id: String,
    pub kind: NoticeKind,
    /// What it is about, with the procedure's boilerplate cut off the front
    /// ([`short_title`]), in the notice's own Dutch.
    pub title: String,
    /// The place in the notice's words: an address, or its area's label.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub place: String,
    /// Publication date, `YYYY-MM-DD`.
    pub date: String,
    /// The last day to respond — object to a decision, comment on an
    /// application — `YYYY-MM-DD`, when the notice states one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub respond_until: Option<String>,
    /// The point to pin it at: the notice's own point, or the middle of its
    /// line or area.
    pub lat: f64,
    pub lng: f64,
    /// The notice's area as one ring of `[lat, lng]`, when it was published as
    /// an area (or a line, kept as its vertices). Empty for a point.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub area: Vec<[f64; 2]>,
    /// Indices into [`PlansFeed::council`] of the council items that name the
    /// same place, most recent first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub council: Vec<u32>,
}

impl Notice {
    /// Whether the response period still runs on `today` (`YYYY-MM-DD`): ISO
    /// dates compare as strings.
    #[must_use]
    pub fn open_on(&self, today: &str) -> bool {
        self.respond_until
            .as_deref()
            .is_some_and(|until| until >= today)
    }
}

/// Where the official text of notice `id` is read.
#[must_use]
pub fn notice_url(id: &str) -> String {
    format!("https://zoek.officielebekendmakingen.nl/{id}.html")
}

/// One meeting a council item was on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CouncilStep {
    /// `YYYY-MM-DD`.
    pub date: String,
    /// The body that met: "gemeenteraad", "Politieke woensdag (Dag agenda)".
    pub body: String,
    /// The meeting's page on the council site.
    pub url: String,
}

/// A document on a council item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CouncilDocument {
    pub name: String,
    pub url: String,
}

/// A place a council item was matched to, by a name in its title.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CouncilPlace {
    /// The name as it stands in the title, which is what was matched.
    pub name: String,
    pub lat: f64,
    pub lng: f64,
}

/// One piece of council business: an agenda item, followed through every
/// meeting it was on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CouncilItem {
    /// The item's title, without the "(raadsvoorstel 14-1-26)" tail it carries
    /// on some agendas and not on others.
    pub title: String,
    /// The meetings it was on, oldest first.
    pub steps: Vec<CouncilStep>,
    pub documents: Vec<CouncilDocument>,
    /// The council has taken a decision on it: there is a "Raadsbesluit".
    #[serde(default)]
    pub decided: bool,
    pub places: Vec<CouncilPlace>,
}

impl CouncilItem {
    /// The date of the latest meeting it was on.
    #[must_use]
    pub fn last_date(&self) -> &str {
        self.steps.last().map_or("", |s| s.date.as_str())
    }
}

/// Everything the layer draws for one municipality.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PlansFeed {
    pub municipality: String,
    /// When atlas built it, seconds since the epoch.
    pub built_unix: u64,
    /// Newest first.
    pub notices: Vec<Notice>,
    pub council: Vec<CouncilItem>,
    /// `[south, west, north, east]` of every notice: where the layer has
    /// anything to say.
    pub bbox: [f64; 4],
}

impl PlansFeed {
    /// Whether `(lat, lng)` lies inside [`Self::bbox`].
    #[must_use]
    pub fn covers(&self, lat: f64, lng: f64) -> bool {
        let [s, w, n, e] = self.bbox;
        lat >= s && lat <= n && lng >= w && lng <= e
    }
}

/// `YYYY-MM-DD` for `unix_secs` seconds since the epoch, UTC — the form every
/// date in the feed is in, so "still open" and "since" are string comparisons.
/// Howard Hinnant's civil-from-days, for the proleptic Gregorian calendar.
#[must_use]
pub fn civil_date(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Whole days from `from` to `to`, both `YYYY-MM-DD`; `None` if either is not
/// a date. Negative when `to` is earlier.
#[must_use]
pub fn days_between(from: &str, to: &str) -> Option<i64> {
    Some(day_number(to)? - day_number(from)?)
}

/// Days since 1970-01-01 of a `YYYY-MM-DD` (days-from-civil, the inverse of
/// [`civil_date`]).
fn day_number(date: &str) -> Option<i64> {
    let mut it = date.split('-').map(str::parse::<i64>);
    let (y, m, d) = (it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// A notice's title without the boilerplate in front of what it is about.
///
/// "Kennisgeving aanvraag omgevingsvergunning reguliere procedure, het vellen
/// van 1 boom, Zwanestraat 27, 9712 CJ Groningen" becomes "het vellen van 1
/// boom, Zwanestraat 27, 9712 CJ Groningen": the kind already says it is an
/// application, and the dot's colour says whether it is still open.
#[must_use]
pub fn short_title(title: &str) -> String {
    let t = title.trim();
    let lower = t.to_lowercase();
    if lower.starts_with("kennisgeving") {
        if let Some(i) = t.find(", ") {
            let rest = t[i + 2..].trim();
            if !rest.is_empty() {
                return capitalise(rest);
            }
        }
    }
    t.to_string()
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| {
        f.to_uppercase().chain(c).collect::<String>()
    })
}

/// A council item's title without a trailing parenthesis, the form the same
/// item is recognised by across meetings: "Uitvoeringskrediet groot onderhoud
/// Vismarkt, Masterplan Warenmarkt (raadsvoorstel 14-1-26)" and its committee
/// listing without the date are one item.
#[must_use]
pub fn council_key(title: &str) -> String {
    let t = title.trim();
    let t = match (t.rfind(" ("), t.ends_with(')')) {
        (Some(i), true) => &t[..i],
        _ => t,
    };
    t.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_are_told_apart_by_their_opening_words() {
        let k = NoticeKind::classify;
        assert_eq!(
            k("omgevingsvergunning", "Kennisgeving aanvraag omgevingsvergunning reguliere procedure, het vellen van 1 boom"),
            NoticeKind::Application
        );
        assert_eq!(
            k("omgevingsvergunning", "Kennisgeving verlenging beslistermijn aanvraag omgevingsvergunning (regulier), het plaatsen"),
            NoticeKind::Application
        );
        assert_eq!(
            k("omgevingsvergunning", "Kennisgeving definitief besluit omgevingsvergunning reguliere procedure (verleend), intern verbouwen"),
            NoticeKind::Decision
        );
        assert_eq!(
            k(
                "omgevingsvergunning",
                "Kennisgeving melding sloopwerkzaamheden, het verwijderen van asbest"
            ),
            NoticeKind::Notification
        );
        assert_eq!(
            k(
                "evenementenvergunning",
                "Toestemming voor Groninger Marktendag"
            ),
            NoticeKind::Event
        );
        assert_eq!(
            k(
                "verkeersbesluit of -mededeling",
                "Verkeersbesluit tot het instellen van een voetpad"
            ),
            NoticeKind::Traffic
        );
        assert_eq!(
            k(
                "ander besluit van algemene strekking",
                "Aanwijsbesluit cameratoezicht Spilsluizen"
            ),
            NoticeKind::Decision
        );
        assert_eq!(
            k("andere voorlichtingsinformatie", "Informatieavond Selwerd"),
            NoticeKind::Other
        );
    }

    #[test]
    fn boilerplate_is_cut_from_the_front() {
        assert_eq!(
            short_title("Kennisgeving aanvraag omgevingsvergunning reguliere procedure, het vellen van 1 boom, Zwanestraat 27, 9712 CJ Groningen"),
            "Het vellen van 1 boom, Zwanestraat 27, 9712 CJ Groningen"
        );
        assert_eq!(
            short_title("Verkeersbesluit tot het instellen van een voetpad"),
            "Verkeersbesluit tot het instellen van een voetpad"
        );
    }

    #[test]
    fn one_item_is_one_key_across_meetings() {
        assert_eq!(
            council_key("Uitvoeringskrediet groot onderhoud Vismarkt, Masterplan Warenmarkt (raadsvoorstel 14-1-26)"),
            "Uitvoeringskrediet groot onderhoud Vismarkt, Masterplan Warenmarkt"
        );
        assert_eq!(
            council_key("Benoeming leden Rekenkamer"),
            "Benoeming leden Rekenkamer"
        );
    }

    #[test]
    fn dates_go_both_ways() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(1_793_318_400), "2026-10-30");
        assert_eq!(days_between("2026-10-30", "2026-12-07"), Some(38));
        assert_eq!(days_between("2026-03-01", "2026-02-28"), Some(-1));
        assert_eq!(days_between("2024-02-28", "2024-03-01"), Some(2));
        assert_eq!(days_between("x", "2026-01-01"), None);
    }

    #[test]
    fn open_is_a_date_comparison() {
        let mut n = Notice {
            id: "gmb-1".into(),
            kind: NoticeKind::Application,
            title: String::new(),
            place: String::new(),
            date: "2026-10-28".into(),
            respond_until: Some("2026-12-07".into()),
            lat: 0.0,
            lng: 0.0,
            area: vec![],
            council: vec![],
        };
        assert!(n.open_on("2026-10-30"));
        assert!(n.open_on("2026-12-07"));
        assert!(!n.open_on("2026-12-08"));
        n.respond_until = None;
        assert!(!n.open_on("2026-10-30"));
    }
}
