//! The app's own updates on screen (`docs/mockups/app-updates`): a pill at the
//! top when a newer build is downloaded, with one tap to install it, and a card
//! over everything when the server refuses this build as too old.
//!
//! The work is `platform::update`'s (on Android, `Updater.java`); this only says
//! where it is and passes the tap on. Ignored, the pill's update still goes in:
//! the updater installs it when the person leaves the app.
//!
//! Both are `Foreground` areas over everything, outside the panel chain, like
//! the calendar's reminder. The too-old card claims the whole screen, so no
//! tap reaches the map or a panel beneath it: nothing there works until the
//! update is in.

use bevy::prelude::*;

use crate::platform::update::{self, UpdateStatus};
use crate::systems::egui_manager::EguiRef;
use crate::systems::{icons, theme};

/// How often the updater is asked: a JNI call, and a download's progress moves
/// visibly at this rate.
const POLL_S: f64 = 0.5;

/// Width of the pill and of the card, fixed for the reason
/// [`crate::systems::place_card`]'s is: a size-to-content `Area` grows otherwise.
const PILL_WIDTH: f32 = 340.0;
const CARD_WIDTH: f32 = 360.0;

#[derive(Resource, Default)]
pub struct AppUpdate {
    status: UpdateStatus,
    polled_at: Option<f64>,
    /// The pill was closed with ×, for this build it offered.
    dismissed: Option<String>,
    /// What a scripted capture shows instead (`--ui updateready`, `updatetooold`,
    /// `updating`), as on a phone; `None` on every other shot and in use.
    pub capture: Option<Capture>,
    /// The component library draws the pill and the card
    /// (`systems::lib_app_update`): this system then draws nothing, but still
    /// asks the updater, publishes what to show ([`Self::lib_view`]) and does
    /// what a tap there asked for ([`Self::lib_action`]).
    pub lib: bool,
    pub lib_view: UpdateView,
    pub lib_action: Option<UpdateAction>,
}

/// A stand-in for the updater and the server's answer, for a capture.
#[derive(Clone, Debug)]
pub struct Capture {
    pub status: UpdateStatus,
    pub too_old: bool,
}

/// What is on screen, in either edition.
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

/// A tap, in either edition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateAction {
    Install,
    /// The pill's ×: not for this build again.
    Dismiss,
}

/// The line under "Installing…" and a download on the too-old card.
pub const CLOSES: &str = "Cartopolis closes when the update is in; a notification opens it again.";

/// The too-old card's text, in either edition: the title and the lines.
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

pub fn render_app_update(egui: EguiRef, mut state: ResMut<AppUpdate>, time: Res<Time<Real>>) {
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
    if state.lib {
        if state.lib_view != view {
            state.lib_view = view;
        }
        return;
    }
    let action = match &view {
        UpdateView::None => None,
        UpdateView::Pill { title, line, ready } => pill(&egui, title, line, *ready),
        UpdateView::TooOld(t) => too_old_card(&egui, t),
    };
    if let Some(action) = action {
        state.act(action);
    }
}

/// "Update ready", with Install — and once tapped, how far it is.
fn pill(egui: &EguiRef, title: &str, line: &str, ready: bool) -> Option<UpdateAction> {
    let ctx = egui.ctx();
    let width = PILL_WIDTH.min(egui.viewport_pts().x - 2.0 * theme::OSD_MARGIN);
    let mut action = None;
    egui::Area::new(egui::Id::new("app_update_pill"))
        .order(egui::Order::Foreground)
        // Under the search bar, which has the top edge.
        .anchor(
            egui::Align2::CENTER_TOP,
            egui::vec2(0.0, crate::systems::dock::reserved_top()),
        )
        .show(ctx, |ui| {
            crate::systems::theme::glass_frame().show(ui, |ui| {
                ui.set_width(width);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(icons::DOWNLOAD)
                            .size(theme::SIZE_TITLE)
                            .color(theme::p().accent),
                    );
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(title)
                                .size(theme::SIZE_BODY)
                                .color(theme::p().cream),
                        );
                        ui.label(
                            egui::RichText::new(line)
                                .size(theme::SIZE_SUB)
                                .color(theme::p().text_dim),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ready {
                            if theme::close_button(ui).clicked() {
                                action = Some(UpdateAction::Dismiss);
                            }
                            if theme::primary_button_sized(ui, "Install", 0.0).clicked() {
                                action = Some(UpdateAction::Install);
                            }
                        } else {
                            ui.add(
                                egui::Spinner::new()
                                    .size(theme::SIZE_BODY)
                                    .color(theme::p().accent),
                            );
                        }
                    });
                });
            });
        });
    action
}

/// "Update Cartopolis to continue": the map dimmed, nothing else answers.
fn too_old_card(egui: &EguiRef, t: &TooOld) -> Option<UpdateAction> {
    let ctx = egui.ctx();
    let screen = ctx.content_rect();
    let width = CARD_WIDTH.min(screen.width() - 2.0 * theme::OSD_MARGIN);
    egui::Area::new(egui::Id::new("app_update_veil"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.painter()
                .rect_filled(screen, 0.0, theme::DARK.ink.gamma_multiply(0.82));
            // Every tap on the veil is taken here, so none reaches the world.
            // unnamed: a blocker under the card, not a control
            ui.allocate_rect(screen, egui::Sense::click_and_drag());
        });
    let mut tapped = false;
    let (heading, text) = too_old_text(t);
    egui::Area::new(egui::Id::new("app_update_card"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            theme::overlay_frame().show(ui, |ui| {
                ui.set_width(width);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new(icons::DOWNLOAD)
                            .size(theme::SIZE_PANEL_TITLE)
                            .color(theme::p().accent),
                    );
                    ui.add_space(4.0);
                    title(ui, heading);
                    if let TooOld::Fetching { fraction, line: mb } = t {
                        ui.add_space(8.0);
                        ui.add(
                            egui::ProgressBar::new(*fraction)
                                .desired_height(8.0)
                                .fill(theme::p().accent),
                        );
                        line(ui, mb);
                    }
                    line(ui, text);
                    if let TooOld::Offer { failed } = t {
                        if let Some(why) = failed {
                            ui.add_space(4.0);
                            let _ = theme::inline_error(ui, &format!("Did not work: {why}"));
                        }
                        ui.add_space(8.0);
                        let label = if failed.is_some() {
                            "Try again"
                        } else {
                            "Update now"
                        };
                        if theme::primary_button_sized(ui, label, ui.available_width()).clicked() {
                            tapped = true;
                        }
                    }
                });
            });
        });
    tapped.then_some(UpdateAction::Install)
}

fn title(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(theme::SIZE_HEADING)
            .color(theme::p().cream),
    );
}

fn line(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(theme::SIZE_SUB)
            .color(theme::p().text_dim),
    );
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
