//! An external GNSS receiver plugged into this machine — an RTK rover such as
//! the UM980 on a USB cable — as a location provider
//! (`docs/notes/rtk-receiver.md`, `cartopolis_gnss`).
//!
//! **It is not a fourth arm of [`crate::platform::location`]; it is in front of
//! all three.** A receiver is something a person plugs in, on a desktop that
//! otherwise has no position at all and on a phone that has a worse one, so
//! [`LocationWatch`](crate::platform::location::LocationWatch) asks here first
//! and falls back to the platform's own provider when there is no fresh fix.
//! That is what lets every consumer — the locate button, driver mode, a chat's
//! location share, the high-precision anchor — take centimetres without one of
//! them changing.
//!
//! The receiver runs for as long as Settings says so, independently of whether
//! anybody is watching: an RTK solution takes tens of seconds to converge after
//! a cold start, and a receiver that only started when the locate button was
//! pressed would spend that time on screen as "searching" every time.
//!
//! The port is the platform's: a tty on the desktop (`cartopolis_gnss::serial`)
//! and the USB host API on Android (`UsbSerial.java`, via
//! [`crate::platform::android::UsbSerial`]). The web build has neither, and
//! everything here answers "nothing plugged in" there.

use crate::platform::location::GpsFix;

/// What the receiver should run with — `systems::gnss_receiver`'s preferences,
/// minus the switch.
// Read only by the native arm; the web build builds one and drops it.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setup {
    /// A serial device on the desktop, or empty for the likeliest one. Unused
    /// on Android, where the first supported USB adapter is the receiver.
    pub port: String,
    pub baud: u32,
    pub configure: bool,
    pub rate_hz: u32,
    pub caster: Option<Caster>,
}

/// The NTRIP caster, as `cartopolis_gnss::Caster` wants it (repeated here so
/// that the web build, which does not link that crate, can name it).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caster {
    pub host: String,
    pub port: u16,
    pub mountpoint: String,
    pub username: String,
    pub password: String,
    pub send_position: bool,
    /// Over TLS (port 443 is regardless).
    pub tls: bool,
}

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
pub use native::ports;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{apply, fix_label, fresh_fix, last_fix, running, set_position_hint, status};

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::sync::Mutex;
    use std::time::Duration;

    use cartopolis_gnss::{Config, Opener, Receiver, Status};

    use super::{Caster, GpsFix, Setup};

    /// A solution older than this is not a position any more: the receiver
    /// writes several a second, so this is a cable pulled or a receiver that
    /// lost the sky, and the platform's provider should answer instead.
    const FIX_MAX_AGE: Duration = Duration::from_secs(3);
    /// Below this ground speed a course over ground is not reported (m/s —
    /// a slow walk).
    const MOVING_MS: f64 = 0.5;

    static RUNNING: Mutex<Option<(Setup, Receiver)>> = Mutex::new(None);

    /// Start, stop or restart the receiver to match `setup` (`None`: off).
    /// Doing nothing when it already matches, so a caller may pass the same
    /// value every frame.
    pub fn apply(setup: Option<&Setup>) {
        let Ok(mut running) = RUNNING.lock() else {
            return;
        };
        if running.as_ref().map(|(s, _)| s) == setup {
            return;
        }
        // The old one stops on drop (its threads notice within a read timeout)
        // and may hold the port for that long; the new one's opener retries.
        *running = None;
        let Some(setup) = setup else {
            tracing::info!("GNSS receiver off");
            return;
        };
        tracing::info!(
            port = %setup.port,
            baud = setup.baud,
            caster = setup.caster.as_ref().map_or("", |c| c.host.as_str()),
            "GNSS receiver on"
        );
        let config = Config {
            configure: setup.configure,
            // What the baud can carry, not only what was asked for.
            rate_hz: cartopolis_gnss::receiver::rate_for_baud(setup.rate_hz, setup.baud),
            caster: setup.caster.as_ref().map(caster),
        };
        let receiver = Receiver::start(opener(setup), config);
        *running = Some((setup.clone(), receiver));
    }

    fn caster(c: &Caster) -> cartopolis_gnss::Caster {
        cartopolis_gnss::Caster {
            host: c.host.clone(),
            port: c.port,
            mountpoint: c.mountpoint.clone(),
            username: c.username.clone(),
            password: c.password.clone(),
            send_position: c.send_position,
            tls: c.tls,
        }
    }

    #[cfg(not(target_os = "android"))]
    fn opener(setup: &Setup) -> Opener {
        let (port, baud) = (setup.port.clone(), setup.baud);
        Box::new(move || cartopolis_gnss::serial::open(&port, baud))
    }

    #[cfg(target_os = "android")]
    fn opener(setup: &Setup) -> Opener {
        use crate::platform::android::UsbSerial;

        struct UsbPort(UsbSerial);
        impl cartopolis_gnss::Port for UsbPort {
            fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
                self.0.read(buf)
            }
            fn write(&mut self, data: &[u8]) -> Result<(), String> {
                self.0.write(data)
            }
        }

        let baud = setup.baud;
        Box::new(move || {
            let (port, name) = UsbSerial::open(baud)?;
            Ok((
                Box::new(UsbPort(port)) as Box<dyn cartopolis_gnss::Port>,
                name,
            ))
        })
    }

    /// Roughly where the receiver is — the map's anchor — for choosing a
    /// correction station before the receiver has a fix
    /// (`cartopolis_gnss::Receiver::set_position_hint`).
    pub fn set_position_hint(hint: Option<(f64, f64)>) {
        if let Ok(running) = RUNNING.lock() {
            if let Some((_, r)) = running.as_ref() {
                r.set_position_hint(hint);
            }
        }
    }

    /// Whether a receiver is switched on (whether or not it is answering).
    #[must_use]
    pub fn running() -> bool {
        RUNNING.lock().is_ok_and(|r| r.is_some())
    }

    /// What the receiver is doing, while it is switched on.
    #[must_use]
    pub fn status() -> Option<Status> {
        RUNNING.lock().ok()?.as_ref().map(|(_, r)| r.status())
    }

    /// The receiver's position, if it has a recent one.
    ///
    /// The course only while moving: a receiver standing still keeps writing
    /// its last course (or whatever its noise points at), and driver mode
    /// prefers a reported course to one it measures.
    #[must_use]
    pub fn fresh_fix() -> Option<GpsFix> {
        let status = status()?;
        Some(to_fix(status.fresh_solution(FIX_MAX_AGE)?))
    }

    /// The receiver's last position, however old — what [`LocationWatch`]
    /// falls back to when neither the receiver nor the platform has anything
    /// newer.
    ///
    /// The platform providers behave this way: a phone's last fix stays
    /// readable after the signal goes, and the consumers decide when it is too
    /// old (`driver::GPS_STALE_S`, from the value no longer changing). Answering
    /// `None` instead, as this did, made every indoor dropout read as "never
    /// had a position": the locate button went back to *Searching…* with the
    /// avatar already standing where the receiver had put it.
    ///
    /// [`LocationWatch`]: crate::platform::location::LocationWatch
    #[must_use]
    pub fn last_fix() -> Option<GpsFix> {
        let status = status()?;
        status.solution.as_ref().map(|s| GpsFix {
            // Not a course any more: it described motion that may have ended.
            heading_deg: None,
            ..to_fix(s)
        })
    }

    fn to_fix(s: &cartopolis_gnss::Solution) -> GpsFix {
        GpsFix {
            lat: s.lat,
            lng: s.lng,
            accuracy_m: s.accuracy_m(),
            speed_ms: s.speed_ms,
            heading_deg: s
                .course_deg
                .filter(|_| s.speed_ms.is_some_and(|v| v > MOVING_MS)),
        }
    }

    /// "RTK fixed", "RTK float", … while the receiver is supplying the position.
    #[must_use]
    pub fn fix_label() -> Option<&'static str> {
        let status = status()?;
        Some(status.fresh_solution(FIX_MAX_AGE)?.quality.label())
    }

    /// Serial ports a receiver could be on, `(path, label)`. Desktop only: on
    /// Android the adapter is found by its USB identity rather than chosen.
    #[cfg(not(target_os = "android"))]
    #[must_use]
    pub fn ports() -> Vec<(String, String)> {
        cartopolis_gnss::serial::ports()
            .into_iter()
            .map(|p| (p.path, p.label))
            .collect()
    }
}

/// The web build: nothing can be plugged in.
#[cfg(target_arch = "wasm32")]
pub fn apply(_setup: Option<&Setup>) {}

#[cfg(target_arch = "wasm32")]
pub fn set_position_hint(_hint: Option<(f64, f64)>) {}

#[cfg(target_arch = "wasm32")]
#[must_use]
pub fn running() -> bool {
    false
}

#[cfg(target_arch = "wasm32")]
#[must_use]
pub fn fresh_fix() -> Option<GpsFix> {
    None
}

#[cfg(target_arch = "wasm32")]
#[must_use]
pub fn last_fix() -> Option<GpsFix> {
    None
}

#[cfg(target_arch = "wasm32")]
#[must_use]
pub fn fix_label() -> Option<&'static str> {
    None
}
