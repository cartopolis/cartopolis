//! The external GNSS receiver's settings, and keeping the running receiver in
//! step with them (`platform::gnss_receiver`, `docs/notes/rtk-receiver.md`).
//!
//! Edited in Settings ▸ General ▸ GNSS receiver and kept in `user.json` beside
//! the other preferences. Not in [`crate::resources::AppPrefs`], which is `Copy`
//! so the panel can edit it by value — a caster's host name and password are
//! strings.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::platform::gnss_receiver::{self, Caster, Setup};
use crate::systems::user_store::UserStore;

/// The receiver as the user set it up.
///
/// Every field defaults to what a fresh install should do, for the reason
/// `user_store`'s `LayerPrefs` gives: a field added later must not fail the
/// parse of a file written before it.
#[derive(Resource, Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct ReceiverPrefs {
    /// Use the receiver at all. Off until asked for: opening serial ports
    /// nobody mentioned is not something an app should do on its own.
    #[serde(default)]
    pub enabled: bool,
    /// A serial device (desktop), or empty for the likeliest USB one.
    #[serde(default)]
    pub port: String,
    /// The UM980 and most receivers leave the factory at 115200.
    #[serde(default = "default_baud")]
    pub baud: u32,
    /// Ask the receiver for position, speed and accuracy at [`Self::rate_hz`].
    #[serde(default = "crate::resources::yes")]
    pub configure: bool,
    #[serde(default = "default_rate")]
    pub rate_hz: u32,
    #[serde(default)]
    pub ntrip: NtripPrefs,
}

/// Where the corrections come from.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct NtripPrefs {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_caster_port")]
    pub port: u16,
    /// Empty: the nearest station in the caster's sourcetable.
    #[serde(default)]
    pub mountpoint: String,
    #[serde(default)]
    pub username: String,
    /// Kept in `user.json` in the clear, as the session tokens beside it are.
    /// An NTRIP password is sent in the clear too (Basic auth over plain TCP,
    /// which is what casters speak), so it is a shared secret of that weight.
    #[serde(default)]
    pub password: String,
    /// Send the receiver's position to the caster. Network (VRS) mountpoints
    /// cannot work without it; a single base does not need it, and when the
    /// nearest station is chosen automatically it is sent only if that
    /// station asks.
    #[serde(default = "crate::resources::yes")]
    pub send_position: bool,
    /// Connect over TLS. Port 443 is TLS whatever this says — it is how
    /// Kadaster's caster is reached, and nothing plain listens there.
    #[serde(default)]
    pub tls: bool,
}

/// Kadaster's public caster: the Netherlands' own reference stations
/// (AGRS.NL, free and open; NETPOS, with an account), on 443 over TLS only.
/// Checked 2026-10-13 — `docs/notes/rtk-receiver.md`.
///
/// **The default caster**, so a receiver switched on in the Netherlands is on
/// corrections with nothing else to set. Elsewhere it does no harm: every
/// station is past `cartopolis_gnss::receiver::MAX_STATION_KM`, so the
/// readout says so and nothing connects.
pub const KADASTER_HOST: &str = "ntrip-lab.kadaster.nl";

impl NtripPrefs {
    /// Never set up: off, with no caster named — the default before Kadaster
    /// was. [`crate::systems::user_store`]'s migration replaces exactly this
    /// with the current default and leaves anything a person typed alone.
    #[must_use]
    pub fn untouched(&self) -> bool {
        !self.enabled
            && self.host.trim().is_empty()
            && self.mountpoint.trim().is_empty()
            && self.username.is_empty()
            && self.password.is_empty()
    }

    /// Point at Kadaster's caster with no login and no mountpoint, so the
    /// nearest open AGRS.NL station is chosen from the first fix (or, before
    /// one, from where the map is).
    pub fn use_kadaster(&mut self) {
        self.enabled = true;
        self.host = KADASTER_HOST.to_string();
        self.port = 443;
        self.tls = true;
        self.mountpoint.clear();
        self.username.clear();
        self.password.clear();
    }
}

fn default_baud() -> u32 {
    115_200
}

fn default_rate() -> u32 {
    5
}

fn default_caster_port() -> u16 {
    2101
}

impl Default for ReceiverPrefs {
    fn default() -> Self {
        Self {
            enabled: false,
            port: String::new(),
            baud: default_baud(),
            configure: true,
            rate_hz: default_rate(),
            ntrip: NtripPrefs::default(),
        }
    }
}

/// Kadaster, switched on — see [`KADASTER_HOST`]. The serde defaults of the
/// individual fields stay the neutral ones (port 2101, no host): they fill in
/// a block somebody wrote, and must not turn it into half a Kadaster setup.
impl Default for NtripPrefs {
    fn default() -> Self {
        let mut n = Self {
            enabled: false,
            host: String::new(),
            port: default_caster_port(),
            mountpoint: String::new(),
            username: String::new(),
            password: String::new(),
            send_position: true,
            tls: false,
        };
        n.use_kadaster();
        n
    }
}

/// Baud rates offered in Settings (not on the web, which has no receiver).
#[cfg(not(target_arch = "wasm32"))]
pub const BAUD_RATES: [u32; 7] = [9_600, 38_400, 57_600, 115_200, 230_400, 460_800, 921_600];
/// Output rates offered in Settings, Hz.
#[cfg(not(target_arch = "wasm32"))]
pub const RATES_HZ: [u32; 5] = [1, 2, 5, 10, 20];

impl ReceiverPrefs {
    /// What the platform should run, or `None` for off.
    #[must_use]
    pub fn setup(&self) -> Option<Setup> {
        if !self.enabled {
            return None;
        }
        let n = &self.ntrip;
        Some(Setup {
            port: self.port.trim().to_string(),
            baud: self.baud,
            configure: self.configure,
            rate_hz: self.rate_hz,
            caster: (n.enabled && !n.host.trim().is_empty()).then(|| Caster {
                host: n.host.trim().to_string(),
                port: n.port,
                mountpoint: n.mountpoint.trim().to_string(),
                username: n.username.trim().to_string(),
                password: n.password.clone(),
                send_position: n.send_position,
                tls: n.tls,
            }),
        })
    }
}

/// Lift the saved settings into the resource, before the first frame applies
/// them.
pub fn load_receiver_prefs(store: Res<UserStore>, mut prefs: ResMut<ReceiverPrefs>) {
    *prefs = store.data.gnss_receiver.clone();
}

/// Start, stop or restart the receiver when the settings change, and write
/// them back to `user.json`. Hands it where the map is, too — the position the
/// nearest correction station is chosen from while the receiver has no fix of
/// its own (indoors, that can be the whole session).
pub fn apply_receiver_prefs(
    prefs: Res<ReceiverPrefs>,
    mut store: ResMut<UserStore>,
    map_anchor: Res<crate::resources::MapAnchor>,
) {
    if prefs.is_changed() {
        gnss_receiver::apply(prefs.setup().as_ref());
        if store.data.gnss_receiver != *prefs {
            store.data.gnss_receiver = prefs.clone();
            store.dirty = true;
        }
    }
    // After the apply: a restarted receiver starts with no hint.
    if prefs.is_changed() || map_anchor.is_changed() {
        gnss_receiver::set_position_hint(map_anchor.geo.map(|(lat, lng, _)| (lat, lng)));
    }
}

/// The receiver's status as lines for the readout, each flagged when it is a
/// problem. Pure, so the sentences are checked without a receiver.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn status_lines(status: &cartopolis_gnss::Status) -> Vec<(bool, String)> {
    use cartopolis_gnss::{CasterState, Link};
    use std::time::Duration;

    let mut out = Vec::new();
    match &status.link {
        Link::Opening => out.push((false, "Looking for the receiver…".to_string())),
        Link::Failed(why) => out.push((true, capitalise(why))),
        Link::Open(name) => out.push((false, format!("Connected: {name}"))),
    }
    if matches!(status.link, Link::Open(_)) {
        if let Some(s) = status.fresh_solution(Duration::from_secs(3)) {
            let mut line = format!(
                "{} · {}",
                s.quality.label(),
                crate::systems::driver::format_accuracy(s.accuracy_m() as f32)
            );
            if let Some(n) = s.satellites {
                line.push_str(&format!(" · {n} satellites"));
            }
            if let Some(h) = s.hdop {
                line.push_str(&format!(" · HDOP {h:.1}"));
            }
            // Measured, so a receiver that ignored the rate it was asked for
            // shows it.
            if let Some(e) = status.epoch_s {
                line.push_str(&format!(" · {:.0} Hz", 1.0 / e));
            }
            out.push((false, line));
            let mut at = format!("{:.8}, {:.8}", s.lat, s.lng);
            if let Some(alt) = s.altitude_m {
                at.push_str(&format!(" · {alt:.2} m above sea level"));
            }
            out.push((false, at));
        } else if status.bytes_in == 0 {
            out.push((false, "Waiting for data…".to_string()));
        } else if status.sentences == 0 && status.bytes_in > 2048 {
            out.push((
                true,
                "Data is arriving, but none of it is NMEA — check the baud rate.".to_string(),
            ));
        } else {
            let n = status.tracking.satellites.unwrap_or(0);
            out.push((false, format!("Searching for a fix · {n} satellites")));
        }
        if let Some(r) = &status.last_reply {
            if !r.contains("OK") {
                out.push((true, format!("The receiver answered: {r}")));
            }
        }
    }
    let c = &status.corrections;
    let mount = c.mountpoint.as_deref().unwrap_or("the caster");
    match &c.state {
        CasterState::Off => {}
        CasterState::WaitingForPosition => out.push((
            false,
            "Corrections: waiting for a position to choose the nearest station".to_string(),
        )),
        CasterState::Connecting => {
            out.push((false, format!("Corrections: connecting to {mount}…")))
        }
        CasterState::Failed(why) => out.push((true, format!("Corrections: {why}"))),
        CasterState::Streaming => {
            let mut line = format!("Corrections: {mount}");
            if let Some(km) = c.distance_km {
                line.push_str(&format!(" ({km:.0} km away)"));
            }
            if c.chosen_from_hint {
                line.push_str(" · chosen from the map until the receiver has a fix");
            }
            line.push_str(&format!(" · {:.0} kB", c.bytes as f64 / 1024.0));
            if let Some(age) = status
                .fresh_solution(Duration::from_secs(3))
                .and_then(|s| s.correction_age_s)
            {
                line.push_str(&format!(" · used {age:.0} s ago"));
            }
            out.push((false, line));
            if !c.messages.is_empty() {
                let list: Vec<String> = c.messages.iter().map(|(m, _)| m.to_string()).collect();
                out.push((false, format!("RTCM {}", list.join(" "))));
            }
        }
    }
    out
}

#[cfg(not(target_arch = "wasm32"))]
fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

/// Everything about the receiver a bug report needs, as text: the settings
/// (never the password), what it is doing now, and its event history — the
/// part a phone's rolling log loses first. Shared by Settings ▸ GNSS receiver ▸
/// *Copy receiver report* and by *Share diagnostics…* (`systems::diagnostics`).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn report(prefs: &ReceiverPrefs) -> String {
    let yes = |b: bool| if b { "yes" } else { "no" };
    let n = &prefs.ntrip;
    let mut out = format!(
        "gnss receiver: {}\n  settings: port {}, {} baud, {} Hz asked ({} Hz carried), set output {}\n  \
         corrections: {} {}:{}{}, mountpoint {}, login {}, send position {}\n",
        if prefs.enabled { "on" } else { "off" },
        if prefs.port.is_empty() { "automatic" } else { prefs.port.as_str() },
        prefs.baud,
        prefs.rate_hz,
        cartopolis_gnss::receiver::rate_for_baud(prefs.rate_hz, prefs.baud),
        yes(prefs.configure),
        if n.enabled { "on" } else { "off" },
        n.host,
        n.port,
        if n.tls || n.port == 443 { " (tls)" } else { "" },
        if n.mountpoint.is_empty() { "(nearest)" } else { n.mountpoint.as_str() },
        yes(!n.username.is_empty()),
        yes(n.send_position),
    );
    let Some(st) = gnss_receiver::status() else {
        out.push_str("  (not running)\n");
        return out;
    };
    for (warn, line) in status_lines(&st) {
        out.push_str(&format!("  {}{line}\n", if warn { "! " } else { "" }));
    }
    let c = &st.corrections;
    out.push_str(&format!(
        "  supplying the position: {}\n  link: {} bytes, {} sentences, {} bad checksums, \
         last byte {}\n  corrections: {} bytes, {} frames, {} held back, last {}\n",
        if gnss_receiver::fresh_fix().is_some() {
            "yes (fresh fix)"
        } else {
            "no"
        },
        st.bytes_in,
        st.sentences,
        st.bad_checksums,
        ago(st.last_byte_at),
        c.bytes,
        c.frames,
        c.held_back,
        ago(c.last_at),
    ));
    if let Some(s) = &st.solution {
        out.push_str(&format!(
            "  last solution: {} {:.8},{:.8} ±{:.3} m, alt {} m, {} sats, HDOP {}, \
             correction age {}, {}\n",
            s.quality.label(),
            s.lat,
            s.lng,
            s.accuracy_m(),
            s.altitude_m.map_or("-".into(), |a| format!("{a:.2}")),
            s.satellites.unwrap_or(0),
            s.hdop.map_or("-".into(), |h| format!("{h:.1}")),
            s.correction_age_s
                .map_or("-".into(), |a| format!("{a:.1} s")),
            ago(st.solution_at),
        ));
    }
    out.push_str("  events (oldest first):\n");
    for line in st.event_log().lines() {
        out.push_str(&format!("    {line}\n"));
    }
    out
}

#[cfg(not(target_arch = "wasm32"))]
fn ago(at: Option<std::time::Instant>) -> String {
    at.map_or("never".into(), |t| {
        format!("{:.1} s ago", t.elapsed().as_secs_f64())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_block_restores_as_the_fresh_install() {
        let p: ReceiverPrefs = serde_json::from_str("{}").unwrap();
        assert_eq!(p, ReceiverPrefs::default());
        assert!(
            p.configure && p.ntrip.send_position,
            "the true defaults are not serde's false"
        );
        assert_eq!(p.baud, 115_200);
        assert_eq!(
            p.ntrip.host, KADASTER_HOST,
            "a fresh install is on Kadaster"
        );
        let partial: NtripPrefs = serde_json::from_str(r#"{"host":"caster.example"}"#).unwrap();
        assert_eq!(
            (partial.port, partial.tls),
            (2101, false),
            "a written block stays neutral"
        );
    }

    #[test]
    fn a_caster_without_a_host_is_no_caster() {
        let mut p = ReceiverPrefs {
            enabled: true,
            ..ReceiverPrefs::default()
        };
        p.ntrip.host.clear();
        assert!(p.setup().unwrap().caster.is_none());
        p.ntrip.host = " caster.example ".into();
        assert_eq!(p.setup().unwrap().caster.unwrap().host, "caster.example");
        p.enabled = false;
        assert!(p.setup().is_none());
    }

    /// The preset reaches Kadaster the only way it can be reached — 443, TLS —
    /// and leaves the station to be chosen, with no login to restrict it to
    /// the stations that need one.
    #[test]
    fn the_kadaster_preset_is_a_working_caster() {
        let mut p = ReceiverPrefs {
            enabled: true,
            ..ReceiverPrefs::default()
        };
        p.ntrip.username = "old".into();
        p.ntrip.use_kadaster();
        let c = p.setup().unwrap().caster.unwrap();
        assert_eq!((c.host.as_str(), c.port, c.tls), (KADASTER_HOST, 443, true));
        assert!(c.mountpoint.is_empty() && c.username.is_empty());
        let old: ReceiverPrefs = serde_json::from_str(r#"{"ntrip":{"host":"x"}}"#).unwrap();
        assert!(!old.ntrip.tls, "a file from before the field is plain");
    }

    /// The report travels off the device — through a share sheet, into a
    /// chat — so it says whether there is a login and never what it is.
    #[test]
    fn the_report_never_carries_the_password() {
        let mut p = ReceiverPrefs {
            enabled: true,
            ..ReceiverPrefs::default()
        };
        p.ntrip.username = "surveyor".into();
        p.ntrip.password = "hunter2-secret".into();
        let r = report(&p);
        assert!(!r.contains("hunter2-secret"), "{r}");
        assert!(r.contains("login yes"), "{r}");
        assert!(r.contains("ntrip-lab.kadaster.nl:443 (tls)"), "{r}");
    }
}
