//! The app's own updates: what the screen needs to say "Update ready" and
//! "Update now" (`systems::app_update`).
//!
//! On Android the work is `Updater.java`'s, which checks cartopolis.org/apk/,
//! downloads and verifies a newer build and hands it to the package installer;
//! this asks it how far it is and tells it when the person tapped. Elsewhere
//! there is nothing to install from inside the app: [`status`] is always
//! [`UpdateStatus::None`] and [`can_install`] is false.
//!
//! [`too_old`] is set by the network thread when the server answers the hello
//! with a newer wire version than this build speaks — the one case in which
//! the app cannot be used until it is updated (`docs/notes/android-build.md`,
//! "The app updates itself").

use std::sync::atomic::{AtomicU16, Ordering};

/// Where the updater is.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum UpdateStatus {
    /// Nothing to tell: up to date, not checked yet, or not on this platform.
    #[default]
    None,
    /// A newer build is downloaded and verified; its version name.
    Ready(String),
    /// Downloading: bytes so far, and the total (0 when the server sent none).
    Fetching { done: u64, total: u64 },
    /// Handed to the installer; the process ends when it is in.
    Installing,
    /// Why the last attempt failed.
    Failed(String),
}

impl UpdateStatus {
    /// `Updater.status()`'s string, read back.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    fn parse(s: &str) -> Self {
        let (word, rest) = s.split_once(' ').unwrap_or((s, ""));
        match word {
            "ready" => Self::Ready(rest.to_string()),
            "fetching" => {
                let mut n = rest.split(' ').map(|v| v.parse().unwrap_or(0));
                Self::Fetching {
                    done: n.next().unwrap_or(0),
                    total: n.next().unwrap_or(0),
                }
            }
            "installing" => Self::Installing,
            "failed" => Self::Failed(rest.to_string()),
            _ => Self::None,
        }
    }
}

/// The server's wire version when it is newer than ours, else 0.
static TOO_OLD: AtomicU16 = AtomicU16::new(0);

/// The server refused this build for speaking an older wire version than its own.
pub fn mark_too_old(server_version: u16) {
    TOO_OLD.store(server_version, Ordering::Relaxed);
}

/// Whether this build is too old for the server it last reached.
pub fn too_old() -> bool {
    TOO_OLD.load(Ordering::Relaxed) != 0
}

/// Whether this platform can install an update from inside the app.
pub fn can_install() -> bool {
    cfg!(target_os = "android")
}

/// Where the updater is. A JNI call on Android: poll it, not every frame.
pub fn status() -> UpdateStatus {
    #[cfg(target_os = "android")]
    {
        match super::android::updater_status() {
            Ok(Some(s)) => UpdateStatus::parse(&s),
            Ok(None) => UpdateStatus::None,
            Err(e) => {
                tracing::warn!(error = %e, "update: no status");
                UpdateStatus::None
            }
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        UpdateStatus::None
    }
}

/// The person tapped Install or Update now.
pub fn install_now() {
    #[cfg(target_os = "android")]
    if let Err(e) = super::android::updater_install_now() {
        tracing::warn!(error = %e, "update: could not start");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_updaters_words() {
        assert_eq!(
            UpdateStatus::parse("ready 1.0.0+abc"),
            UpdateStatus::Ready("1.0.0+abc".into())
        );
        assert_eq!(
            UpdateStatus::parse("fetching 34 80"),
            UpdateStatus::Fetching {
                done: 34,
                total: 80
            }
        );
        assert_eq!(UpdateStatus::parse("installing"), UpdateStatus::Installing);
        assert_eq!(
            UpdateStatus::parse("failed HTTP 404"),
            UpdateStatus::Failed("HTTP 404".into())
        );
        assert_eq!(UpdateStatus::parse("what"), UpdateStatus::None);
    }
}
