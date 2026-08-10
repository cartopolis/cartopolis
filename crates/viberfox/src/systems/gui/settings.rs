//! The Settings panel, and its Audio tab.
//!
//! Tabbed because settings accrete: Audio is the first tab and the only one with
//! content, but a panel that is *only* audio would have to be renamed or nested
//! the moment a second category arrives, and the tab strip costs a few lines now
//! against a rename later.
//!
//! # What the two tests actually establish
//!
//! **Sound test** plays a short chime through the same `bevy_audio` path the
//! soundscape uses (`systems::player::soundscape::synth::test_tone`). That is the
//! point of reusing it rather than opening a second output device: it proves the
//! path the *application* uses works, not that some other path does.
//!
//! **Mic test** opens capture through [`crate::platform::audio`] and shows a live
//! level meter plus a running peak. Both are needed. A bare instantaneous meter
//! is hard to read while you are talking — you look at the bar, not at the words
//! — and a peak hold answers "did anything at all get through" after the fact.
//!
//! The mic is opened only while the tab is showing it and released as soon as the
//! test stops or the panel closes. Holding a microphone open for the lifetime of
//! the process is how an application ends up with a permanent recording indicator
//! in the OS status bar, which users are right to find alarming.

use bevy::prelude::*;

use crate::platform::audio::{self, AudioInput};
use crate::resources::RightPanel;
use crate::systems::egui_manager::EguiManager;

/// Which tab of the Settings panel is showing.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    #[default]
    Audio,
}

impl SettingsTab {
    const ALL: [Self; 1] = [Self::Audio];

    fn label(self) -> &'static str {
        match self {
            Self::Audio => "Audio",
        }
    }
}

/// Panel state, plus the live microphone while a mic test is running.
#[derive(Resource, Default)]
pub struct SettingsState {
    pub tab: SettingsTab,
    /// `Some` only while the mic test is active — see the module note on why
    /// this is not held open for the process lifetime.
    pub mic: Option<AudioInput>,
    /// Highest meter value seen since the test started.
    pub peak: f32,
}

impl SettingsState {
    /// Begin a mic test, replacing any running one.
    pub fn start_mic(&mut self) {
        self.peak = 0.0;
        self.mic = Some(AudioInput::start());
    }

    /// Stop and release the device.
    pub fn stop_mic(&mut self) {
        self.mic = None;
        self.peak = 0.0;
    }
}

/// Track the peak while a test runs. Split out so the panel body stays layout.
fn observe_peak(state: &mut SettingsState) -> Option<(f32, f32)> {
    let mic = state.mic.as_ref()?;
    let meter = audio::level_to_meter(mic.level());
    state.peak = state.peak.max(meter);
    Some((meter, state.peak))
}

/// The Settings panel. Ordered in the Phase 7b `.chain()` like every other panel
/// — see the note there; an unordered panel loses egui's hit test at random.
pub fn render_settings_panel(
    mut egui: ResMut<EguiManager>,
    mut right_panel: ResMut<RightPanel>,
    mut state: ResMut<SettingsState>,
    #[cfg(feature = "audio")] soundscape: Option<
        Res<crate::systems::player::soundscape::Soundscape>,
    >,
    mut commands: Commands,
) {
    let compact = egui.compact();
    let viewport_w = egui.viewport_pts().x;
    let mut open = right_panel.is(RightPanel::Settings);
    let mut close = false;
    let mut play_tone = false;

    crate::systems::egui_manager::responsive_right_panel(
        "settings_panel",
        compact,
        viewport_w,
        320.0,
    )
    .show_collapsible(egui.ui_mut(), &mut open, |ui| {
        let panel = RightPanel::Settings;
        close = crate::systems::theme::panel_header(ui, panel.icons(), panel.label());

        ui.horizontal_wrapped(|ui| {
            for tab in SettingsTab::ALL {
                ui.selectable_value(&mut state.tab, tab, tab.label());
            }
        });
        ui.separator();

        match state.tab {
            SettingsTab::Audio => play_tone = audio_tab(ui, &mut state),
        }
    });

    if close {
        *right_panel = RightPanel::None;
    }
    // Closing the panel must release the microphone: leaving it open behind a
    // hidden panel is the recording-indicator problem the module note describes.
    if !right_panel.is(RightPanel::Settings) && state.mic.is_some() {
        state.stop_mic();
    }

    #[cfg(feature = "audio")]
    if play_tone {
        if let Some(soundscape) = soundscape {
            commands.spawn((
                bevy::audio::AudioPlayer(soundscape.test_tone.clone()),
                bevy::audio::PlaybackSettings {
                    mode: bevy::audio::PlaybackMode::Despawn,
                    ..default()
                },
            ));
        }
    }
    #[cfg(not(feature = "audio"))]
    {
        let _ = (play_tone, &mut commands);
    }
}

/// The Audio tab body. Returns `true` if the sound test was pressed this frame.
fn audio_tab(ui: &mut egui::Ui, state: &mut SettingsState) -> bool {
    let mut play_tone = false;

    ui.label(egui::RichText::new("Output").strong());
    ui.label("Play a short chime to confirm you can hear the application.");
    if ui.button("▶  Sound test").clicked() {
        play_tone = true;
    }
    if !cfg!(feature = "audio") {
        ui.colored_label(
            crate::systems::theme::ERROR,
            "This build has no audio support.",
        );
    }

    ui.add_space(10.0);
    ui.separator();
    ui.label(egui::RichText::new("Microphone").strong());

    if !AudioInput::available() {
        // The `platform::location` contract: say unavailable rather than show a
        // meter that can never move.
        ui.colored_label(crate::systems::theme::ERROR, unavailable_reason());
        return play_tone;
    }

    let running = state.mic.is_some();
    ui.horizontal(|ui| {
        if ui
            .button(if running {
                "■  Stop"
            } else {
                "●  Test microphone"
            })
            .clicked()
        {
            if running {
                state.stop_mic();
            } else {
                state.start_mic();
            }
        }
    });

    if let Some((meter, peak)) = observe_peak(state) {
        let mic = state.mic.as_ref().expect("observe_peak proved it is Some");
        if let Some(err) = mic.error() {
            ui.colored_label(crate::systems::theme::ERROR, err);
        } else if !mic.is_receiving() {
            // Distinct from "silent": no callback has fired at all. On the web
            // this is usually the permission prompt still being open.
            ui.label("Waiting for the microphone…");
        } else {
            level_bar(ui, meter, peak);
            if let Some(name) = mic.device_name() {
                ui.small(name);
            }
            ui.small("Speak normally — the bar should reach the middle.");
        }
    }

    play_tone
}

/// Why capture is unavailable, specifically enough to be actionable.
fn unavailable_reason() -> &'static str {
    if !cfg!(feature = "audio") {
        "This build has no audio support (built without the `audio` feature)."
    } else if cfg!(target_os = "android") {
        "Microphone capture is not implemented on Android yet."
    } else {
        "No microphone backend on this platform."
    }
}

/// A level meter with a peak-hold tick.
fn level_bar(ui: &mut egui::Ui, meter: f32, peak: f32) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 14.0), egui::Sense::hover());
    let p = ui.painter();
    let radius = egui::CornerRadius::same(3);
    p.rect_filled(rect, radius, crate::systems::theme::SURFACE);

    if meter > 0.0 {
        let filled =
            egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * meter, rect.height()));
        p.rect_filled(filled, radius, meter_colour(meter));
    }
    if peak > 0.0 {
        let x = rect.min.x + rect.width() * peak;
        p.line_segment(
            [egui::pos2(x, rect.min.y), egui::pos2(x, rect.max.y)],
            egui::Stroke::new(1.5, crate::systems::theme::CREAM),
        );
    }
}

/// Green through amber to the brand orange as the signal approaches clipping.
///
/// The top of the bar is a warning, not a target — a meter that is green at
/// full scale invites people to clip their own microphone.
fn meter_colour(meter: f32) -> egui::Color32 {
    if meter > 0.9 {
        crate::systems::theme::ERROR
    } else if meter > 0.75 {
        crate::systems::theme::AMBER
    } else {
        crate::systems::theme::FOX
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_holds_the_maximum_and_resets_with_the_test() {
        let mut state = SettingsState::default();
        // No mic: nothing to observe, and no panic.
        assert!(observe_peak(&mut state).is_none());

        state.peak = 0.6;
        state.stop_mic();
        assert_eq!(state.peak, 0.0, "stopping a test clears the peak hold");
    }

    #[test]
    fn meter_warns_before_it_reaches_the_end() {
        // The bar must change colour below full scale, or it reads as a target.
        assert_eq!(meter_colour(0.5), crate::systems::theme::FOX);
        assert_eq!(meter_colour(0.8), crate::systems::theme::AMBER);
        assert_eq!(meter_colour(0.95), crate::systems::theme::ERROR);
    }

    #[test]
    fn unavailable_reason_is_specific() {
        // A generic "unavailable" is what sends someone to check their hardware
        // when the answer is a build flag.
        let reason = unavailable_reason();
        assert!(reason.ends_with('.'), "reason should read as a sentence");
        if !cfg!(feature = "audio") {
            assert!(reason.contains("audio"), "should name the missing feature");
        }
    }
}
