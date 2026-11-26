//! The app's own updates on screen (`docs/mockups/app-updates`): a pill at the
//! top when a newer build is downloaded, with one tap to install it, and a card
//! over everything when the server refuses this build as too old.
//!
//! The work is `platform::update`'s (on Android, `Updater.java`); this only says
//! where it is and passes the tap on. Ignored, the pill's update still goes in:
//! the updater installs it when the person leaves the app.
//!
//! The component library draws both (`systems::lib_app_update`);
//! [`drive_app_update`] asks the updater, publishes what shows and does what a
//! tap there asked for. The too-old card claims the whole screen, so no tap
//! reaches the map or a panel beneath it: nothing there works until the update
//! is in.

use bevy::prelude::*;

use crate::platform::update::{self, UpdateStatus};

/// How often the updater is asked: a JNI call, and a download's progress moves
/// visibly at this rate.
const POLL_S: f64 = 0.5;

#[derive(Resource, Default)]
pub struct AppUpdate {
    status: UpdateStatus,
    polled_at: Option<f64>,
    /// The pill was closed with ×, for this build it offered.
    dismissed: Option<String>,
    /// What a scripted capture shows instead (`--ui updateready`, `updatetooold`,
    /// `updating`), as on a phone; `None` on every other shot and in use.
    pub capture: Option<Capture>,
    /// What to show, for the library's edition, which hands a tap back in
    /// [`Self::lib_action`].
    pub lib_view: UpdateView,
    pub lib_action: Option<UpdateAction>,
}
/// A stand-in for the updater and the server's answer, for a capture.
#[derive(Clone, Debug)]
pub struct Capture {
    pub status: UpdateStatus,
    pub too_old: bool,
}

/// What is on screen.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum UpdateView {
    #[default]
    None,
    /// The pill at the top: "Update ready" with Install and ×, or how far
    /// the update is once tapped.
    Pill {
        title: &'static str,
        line: String,
        ready: bool,
    },
    /// The card over everything: this build is too old for the server.
    TooOld(TooOld),
}

/// The too-old card's states.
#[derive(Clone, Debug, PartialEq)]
pub enum TooOld {
    /// Downloading: the fraction done, and "34 of 80 MB".
    Fetching {
        fraction: f32,
        line: String,
    },
    Installing,
    /// The update can be had here; why the last attempt failed, if it did.
    Offer {
        failed: Option<String>,
    },
    /// This build cannot update itself: get the new one from the site.
    Elsewhere,
}

/// A tap on the pill or the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateAction {
    Install,
    /// The pill's ×: not for this build again.
    Dismiss,
}

/// The line under "Installing…" and a download on the too-old card.
pub const CLOSES: &str = "Cartopolis closes when the update is in; a notification opens it again.";

/// The too-old card's text: the title and the lines.
#[must_use]
pub fn too_old_text(t: &TooOld) -> (&'static str, &'static str) {
    match t {
        TooOld::Fetching { .. } => ("Updating…", CLOSES),
        TooOld::Installing => ("Installing…", CLOSES),
        TooOld::Offer { .. } => (
            "Update Cartopolis to continue",
            "The server has moved on and this version can no longer \
             talk to it. The update takes about a minute.",
        ),
        TooOld::Elsewhere => (
            "This version of Cartopolis is too old",
            "The server has moved on and this version can no longer \
             talk to it. Get the new one from cartopolis.org.",
        ),
    }
}

impl AppUpdate {
    /// What to show: the pill, the too-old card, or nothing.
    fn view(&self, too_old: bool, installable: bool) -> UpdateView {
        if too_old {
            return UpdateView::TooOld(match &self.status {
                UpdateStatus::Fetching { done, total } => TooOld::Fetching {
                    fraction: if *total > 0 {
                        #[allow(clippy::cast_precision_loss)]
                        let f = *done as f32 / *total as f32;
                        f
                    } else {
                        0.0
                    },
                    line: megabytes(*done, *total),
                },
                UpdateStatus::Installing => TooOld::Installing,
                _ if installable => TooOld::Offer {
                    failed: match &self.status {
                        UpdateStatus::Failed(why) => Some(why.clone()),
                        _ => None,
                    },
                },
                _ => TooOld::Elsewhere,
            });
        }
        let (title, line) = match &self.status {
            UpdateStatus::Ready(name) if self.dismissed.as_ref() != Some(name) => {
                ("Update ready", format!("Cartopolis {name}"))
            }
            UpdateStatus::Fetching { done, total } if self.dismissed.is_none() => {
                ("Updating…", megabytes(*done, *total))
            }
            UpdateStatus::Installing if self.dismissed.is_none() => {
                ("Installing…", "Cartopolis closes when it is in".to_string())
            }
            _ => return UpdateView::None,
        };
        UpdateView::Pill {
            title,
            line,
            ready: matches!(self.status, UpdateStatus::Ready(_)),
        }
    }

    /// Do what a tap asked for.
    fn act(&mut self, action: UpdateAction) {
        match action {
            UpdateAction::Install => update::install_now(),
            UpdateAction::Dismiss => {
                if let UpdateStatus::Ready(name) = &self.status {
                    self.dismissed = Some(name.clone());
                }
            }
        }
    }
}

/// Asks the updater, publishes what shows, and does what a tap asked for.
pub fn drive_app_update(mut state: ResMut<AppUpdate>, time: Res<Time<Real>>) {
    // A tap the library's edition handed back.
    if let Some(action) = state.lib_action {
        state.lib_action = None;
        state.act(action);
    }
    let (too_old, installable) = if let Some(c) = state.capture.clone() {
        state.status = c.status;
        (c.too_old, true)
    } else {
        let now = time.elapsed_secs_f64();
        if update::can_install() && state.polled_at.is_none_or(|t| now - t >= POLL_S) {
            state.polled_at = Some(now);
            state.status = update::status();
        }
        (update::too_old(), update::can_install())
    };
    let view = state.view(too_old, installable);
    if state.lib_view != view {
        state.lib_view = view;
    }
}

/// "34 of 80 MB", or "34 MB" when the server sent no length.
fn megabytes(done: u64, total: u64) -> String {
    const MB: u64 = 1024 * 1024;
    if total > 0 {
        format!("{} of {} MB", done / MB, total.div_ceil(MB))
    } else {
        format!("{} MB", done / MB)
    }
}
