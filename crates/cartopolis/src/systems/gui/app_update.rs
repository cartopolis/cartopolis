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
}

/// A stand-in for the updater and the server's answer, for a capture.
#[derive(Clone, Debug)]
pub struct Capture {
    pub status: UpdateStatus,
    pub too_old: bool,
}

pub fn render_app_update(egui: EguiRef, mut state: ResMut<AppUpdate>, time: Res<Time<Real>>) {
    if let Some(c) = state.capture.clone() {
        if c.too_old {
            too_old_card(&egui, &c.status, true);
        } else {
            state.status = c.status;
            pill(&egui, &mut state);
        }
        return;
    }
    let too_old = update::too_old();
    let now = time.elapsed_secs_f64();
    if update::can_install() && state.polled_at.is_none_or(|t| now - t >= POLL_S) {
        state.polled_at = Some(now);
        state.status = update::status();
    }
    if too_old {
        too_old_card(&egui, &state.status, update::can_install());
    } else {
        pill(&egui, &mut state);
    }
}

/// "Update ready", with Install — and once tapped, how far it is.
fn pill(egui: &EguiRef, state: &mut AppUpdate) {
    let (title, line) = match &state.status {
        UpdateStatus::Ready(name) if state.dismissed.as_ref() != Some(name) => {
            ("Update ready", format!("Cartopolis {name}"))
        }
        UpdateStatus::Fetching { done, total } if state.dismissed.is_none() => {
            ("Updating…", megabytes(*done, *total))
        }
        UpdateStatus::Installing if state.dismissed.is_none() => {
            ("Installing…", "Cartopolis closes when it is in".to_string())
        }
        _ => return,
    };
    let ready = matches!(state.status, UpdateStatus::Ready(_));
    let ctx = egui.ctx();
    let width = PILL_WIDTH.min(egui.viewport_pts().x - 2.0 * theme::OSD_MARGIN);
    let mut install = false;
    let mut close = false;
    egui::Area::new(egui::Id::new("app_update_pill"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, theme::OSD_MARGIN))
        .show(ctx, |ui| {
            crate::systems::chats::glass_frame().show(ui, |ui| {
                ui.set_width(width);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(icons::DOWNLOAD)
                            .size(theme::SIZE_TITLE)
                            .color(theme::ACCENT),
                    );
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(title)
                                .size(theme::SIZE_BODY)
                                .color(theme::CREAM),
                        );
                        ui.label(
                            egui::RichText::new(&line)
                                .size(theme::SIZE_SUB)
                                .color(theme::TEXT_DIM),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ready {
                            if theme::close_button(ui).clicked() {
                                close = true;
                            }
                            if theme::primary_button_sized(ui, "Install", 0.0).clicked() {
                                install = true;
                            }
                        } else {
                            ui.add(
                                egui::Spinner::new()
                                    .size(theme::SIZE_BODY)
                                    .color(theme::ACCENT),
                            );
                        }
                    });
                });
            });
        });
    if install {
        update::install_now();
    }
    if close {
        if let UpdateStatus::Ready(name) = &state.status {
            state.dismissed = Some(name.clone());
        }
    }
}

/// "Update Cartopolis to continue": the map dimmed, nothing else answers.
fn too_old_card(egui: &EguiRef, status: &UpdateStatus, installable: bool) {
    let ctx = egui.ctx();
    let screen = ctx.content_rect();
    let width = CARD_WIDTH.min(screen.width() - 2.0 * theme::OSD_MARGIN);
    egui::Area::new(egui::Id::new("app_update_veil"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.painter()
                .rect_filled(screen, 0.0, theme::INK.gamma_multiply(0.82));
            // Every tap on the veil is taken here, so none reaches the world.
            // unnamed: a blocker under the card, not a control
            ui.allocate_rect(screen, egui::Sense::click_and_drag());
        });
    let mut tapped = false;
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
                            .color(theme::ACCENT),
                    );
                    ui.add_space(4.0);
                    match status {
                        UpdateStatus::Fetching { done, total } => {
                            title(ui, "Updating…");
                            ui.add_space(8.0);
                            let fraction = if *total > 0 {
                                #[allow(clippy::cast_precision_loss)]
                                let f = *done as f32 / *total as f32;
                                f
                            } else {
                                0.0
                            };
                            ui.add(egui::ProgressBar::new(fraction)
                                    .desired_height(8.0)
                                    .fill(theme::ACCENT));
                            line(ui, &megabytes(*done, *total));
                            line(ui, "Cartopolis closes when the update is in; a notification opens it again.");
                        }
                        UpdateStatus::Installing => {
                            title(ui, "Installing…");
                            line(ui, "Cartopolis closes when the update is in; a notification opens it again.");
                        }
                        _ if installable => {
                            title(ui, "Update Cartopolis to continue");
                            line(
                                ui,
                                "The server has moved on and this version can no longer \
                                 talk to it. The update takes about a minute.",
                            );
                            if let UpdateStatus::Failed(why) = status {
                                ui.add_space(4.0);
                                let _ = theme::inline_error(ui, &format!("Did not work: {why}"));
                            }
                            ui.add_space(8.0);
                            let label = if matches!(status, UpdateStatus::Failed(_)) {
                                "Try again"
                            } else {
                                "Update now"
                            };
                            if theme::primary_button_sized(ui, label, ui.available_width())
                                .clicked()
                            {
                                tapped = true;
                            }
                        }
                        _ => {
                            title(ui, "This version of Cartopolis is too old");
                            line(
                                ui,
                                "The server has moved on and this version can no longer \
                                 talk to it. Get the new one from cartopolis.org.",
                            );
                        }
                    }
                });
            });
        });
    if tapped {
        update::install_now();
    }
}

fn title(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(theme::SIZE_HEADING)
            .color(theme::CREAM),
    );
}

fn line(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(theme::SIZE_SUB)
            .color(theme::TEXT_DIM),
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
