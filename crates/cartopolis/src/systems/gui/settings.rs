//! The Settings panel — and the About panel, which is next to it because they
//! are the two screens that are *about the application* rather than about the
//! world.
//!
//! # Settings
//!
//! Five tabs: General, Map, Graphics, Audio, Account. It began as one (Audio),
//! which made a panel called "Settings" that was really a hardware diagnostic —
//! the two things behind it were a chime and a level meter, and every actual
//! preference in the client lived somewhere else: the map layers in their own
//! panel, the sky in the HUD switcher and the dev panel, the render budget
//! nowhere at all.
//!
//! **Graphics is the last of that migration** and the one with a cost attached.
//! Shadows, the sky, sky reflections and the stats overlay were `DevPanelState`
//! fields, reached by a keyboard chord — on a client whose main platform is a
//! phone, which has no keyboard and no developer panel worth opening. So the
//! settings a struggling device most needs were the ones its owner could not
//! reach. They were also unpersisted, which made every "restart to apply" toggle
//! a lie: the restart that applied it also discarded it.
//!
//! **Nothing here invents a backend.** Each control is wired to state that
//! already exists, or to a [`crate::resources::AppPrefs`] field whose doc comment
//! says plainly that nothing reads it yet. The alternative — a switch that
//! appears to work — is worse than an honest one that says "next launch", because
//! the first is a bug report and the second is a fact.
//!
//! # What the two audio tests actually establish
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
//!
//! # About
//!
//! The version, the build, and the third-party notices — **a licence obligation,
//! not a courtesy**. ODbL (OpenStreetMap and everything derived from it), CC BY
//! 4.0 (3DBAG, MET Norway) and the SIL Open Font License all require the notice
//! to travel with the distributed work, and before this panel existed the app's
//! only attribution anywhere was three `ui.small()` lines at the foot of the
//! Layers panel — which credited neither VersaTiles, nor AHN, nor Quaternius, nor
//! the font.
//!
//! The credits are not typed here. `LICENSE-THIRD-PARTY.md` at the repo root says
//! of itself that it is the canonical list and that "the in-app About panel
//! renders it", so this panel `include_str!`s that file and parses its records
//! ([`parse_notices`]). Adding a data source therefore means editing one file,
//! and a source that is credited in the tarball but not in the app becomes
//! impossible rather than merely unlikely.

use bevy::prelude::*;

use crate::platform::audio::{self, AudioInput};
use crate::resources::{AppPrefs, QualityPreset, RightPanel, SunMode, Units};
use crate::systems::egui_manager::EguiManager;
use crate::systems::icons;
use crate::systems::theme;

/// Which tab of the Settings panel is showing.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsTab {
    #[default]
    General,
    Map,
    /// What this machine spends on the picture. Everything here used to be in the
    /// developer panel, where it was both unreachable to the people who needed it
    /// most and unpersisted — see [`graphics_tab`].
    Graphics,
    Audio,
    Account,
}

impl SettingsTab {
    const ALL: [Self; 5] = [
        Self::General,
        Self::Map,
        Self::Graphics,
        Self::Audio,
        Self::Account,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Map => "Map",
            Self::Graphics => "Graphics",
            Self::Audio => "Audio",
            Self::Account => "Account",
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
    /// The password re-entered to confirm account deletion.
    ///
    /// Cleared the moment the request goes out and whenever the panel closes: it
    /// is the one string in this struct that must not outlive the interaction
    /// that produced it.
    pub delete_password: String,
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
///
/// `gain` is [`AppPrefs::capture_gain`], applied here because the meter is the
/// only place the input gain can be judged — voice transmits the gained signal,
/// so a meter showing the raw one would have someone calibrate against a number
/// nobody sends.
fn observe_peak(state: &mut SettingsState, gain: f32) -> Option<(f32, f32)> {
    let mic = state.mic.as_ref()?;
    let meter = audio::level_to_meter((mic.level() * gain).clamp(0.0, 1.0));
    state.peak = state.peak.max(meter);
    Some((meter, state.peak))
}

/// The Settings panel. Ordered in the Phase 7b `.chain()` like every other panel
/// — see the note there; an unordered panel loses egui's hit test at random.
#[allow(clippy::too_many_arguments)]
pub fn render_settings_panel(
    mut egui: ResMut<EguiManager>,
    mut right_panel: ResMut<RightPanel>,
    mut state: ResMut<SettingsState>,
    mut prefs: ResMut<AppPrefs>,
    mut dev: ResMut<crate::resources::DevPanelState>,
    #[cfg(feature = "audio")] soundscape: Option<
        Res<crate::systems::player::soundscape::Soundscape>,
    >,
    windows: Query<&Window>,
    mut commands: Commands,
    mut identity: ResMut<crate::resources::AuthIdentity>,
    online: Option<Res<crate::resources::OnlineSession>>,
    // The sky this run actually got, so the Graphics tab can report it beside
    // the preference. `Option` because it is inserted by `setup_camera`, which
    // needs an adapter — a headless world without one has no answer, and saying
    // so beats implying the preference took effect.
    sky: Option<Res<crate::systems::sky_dome::SkyMode>>,
) {
    // Physical pixels, for the render-scale row's "drawn at W x H" hint. `None` in
    // a headless `--shot` run, where there is no window and the hint degrades to a
    // sentence without numbers rather than inventing a resolution.
    let target = windows.iter().next().map(|w| {
        UVec2::new(
            w.resolution.physical_width(),
            w.resolution.physical_height(),
        )
    });
    let mut play_tone = false;
    let mut delete_account = false;

    // Edited as plain values and written back only on a real change. Both of
    // these resources are read by `is_changed()` gates — `AppPrefs` will be, once
    // it is persisted, and `DevPanelState`'s gate re-runs the whole day/night
    // lighting recompute — and `ui.selectable_value(&mut res.field, …)` takes a
    // `DerefMut`, i.e. marks the resource changed on *every frame the panel is
    // open*. This is the same shape `hud::render_status_hud` uses for the sun
    // switcher and for the same reason.
    let mut edited = *prefs;
    let mut sun_mode = dev.sun_mode;

    crate::systems::egui_manager::right_panel(
        &mut egui,
        &mut right_panel,
        RightPanel::Settings,
        "settings_panel",
        320.0,
        |ui| {
            ui.horizontal_wrapped(|ui| {
                for tab in SettingsTab::ALL {
                    ui.selectable_value(&mut state.tab, tab, tab.label());
                }
            });
            ui.separator();

            // Every tab is a list that can outgrow a phone's 609 pt of viewport, and
            // a `Panel` clips rather than scrolls (the Layers panel had lost its last
            // ten rows to exactly this). The header stays outside, so the close
            // button cannot scroll away.
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match state.tab {
                    SettingsTab::General => general_tab(ui, &mut edited),
                    SettingsTab::Map => map_tab(ui, &mut edited, &mut sun_mode),
                    SettingsTab::Graphics => {
                        graphics_tab(ui, &mut edited, target, sky.as_deref().copied());
                    }
                    SettingsTab::Audio => play_tone = audio_tab(ui, &mut state, &mut edited),
                    SettingsTab::Account => {
                        delete_account =
                            account_tab(ui, &mut state, &mut identity, online.is_some());
                    }
                });
        },
    );

    if edited != *prefs {
        *prefs = edited;
    }
    if sun_mode != dev.sun_mode {
        dev.sun_mode = sun_mode;
    }
    // Closing the panel must release the microphone: leaving it open behind a
    // hidden panel is the recording-indicator problem the module note describes.
    if !right_panel.is(RightPanel::Settings) && state.mic.is_some() {
        state.stop_mic();
    }
    // …and it must drop the typed password, for the same shape of reason.
    if !right_panel.is(RightPanel::Settings) && !state.delete_password.is_empty() {
        state.delete_password.clear();
    }

    if delete_account {
        if let Some(online) = online {
            online.send(
                "delete account",
                cartopolis_core::NetMessage::DeleteAccount {
                    request_id: crate::resources::DELETE_ACCOUNT_REQUEST,
                    password: std::mem::take(&mut state.delete_password),
                },
            );
        }
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

/// A section title. Delegates to `theme::section`, which is where this idiom
/// lives now — it was the only *named* section helper in the client while four
/// other panels each invented their own, so it was promoted rather than copied.
fn section(ui: &mut egui::Ui, title: &str) {
    theme::section(ui, title);
}

/// Every binding the client has, grouped by the mode it works in.
///
/// **There was no control legend anywhere in the application.** WASD, Space, F,
/// E, Q/R, Shift, Alt, 1–6, Tab, Delete and three Ctrl chords existed only in
/// `CLAUDE.md` and in commit bodies; the sole in-UI hint was the assistant's
/// "(Ctrl+Enter)" and the `[E]` on the interact prompt. So a desktop user who
/// never guessed a key saw an avatar they could not move, and the keys that are
/// *silently* mode-dependent — Space and the emotes do nothing outside Follow, E
/// means "interact" in Follow and "up" in Fly — had nothing to explain
/// themselves.
///
/// It lives in About because About already exists, is already in the main menu,
/// already scrolls, and ships on the phone — where the gesture half of this
/// table is the part that matters. A new panel for it would be a new thing to
/// discover, which is the problem rather than the fix.
///
/// Grouped **by mode** deliberately: a flat list would say `E` twice and be
/// wrong both times.
fn controls_section(ui: &mut egui::Ui) {
    theme::section(ui, "Controls");
    let row = |ui: &mut egui::Ui, keys: &str, what: &str| {
        ui.horizontal(|ui| {
            ui.add_sized(
                [86.0, ui.spacing().interact_size.y],
                egui::Label::new(
                    egui::RichText::new(keys)
                        .size(theme::SIZE_SUB)
                        .color(theme::ACCENT_TEXT),
                )
                .wrap(),
            );
            ui.add(
                egui::Label::new(
                    egui::RichText::new(what)
                        .size(theme::SIZE_SUB)
                        .color(theme::TEXT),
                )
                .wrap(),
            );
        });
    };

    theme::note(ui, "Walking (the default camera)");
    row(
        ui,
        "W A S D",
        "Walk — the avatar turns to face where it goes",
    );
    row(ui, "Shift / Alt", "Run / stroll");
    row(ui, "Space", "Jump; hold to take off");
    row(ui, "F", "Toggle flight");
    row(ui, "R / Q", "Climb / descend, while flying");
    row(ui, "E", "Interact with whatever is in front of you");
    row(
        ui,
        "1 – 6",
        "Emotes: wave, interact, roll, punch, kick, faint",
    );

    ui.add_space(4.0);
    theme::note(ui, "Flying");
    row(ui, "W A S D", "Move the camera");
    row(ui, "E / Q", "Up / down");
    row(ui, "Shift", "Move faster");

    ui.add_space(4.0);
    theme::note(ui, "Anywhere");
    row(ui, "Tab", "Next camera: walk → fly → whole Earth");
    row(ui, "Esc", "Close whatever is open, innermost first");
    row(ui, "Ctrl+L", "Search");
    row(ui, "Ctrl+Shift+A", "AI assistant");
    row(ui, "Ctrl+Shift+D", "Developer tools");

    ui.add_space(4.0);
    theme::note(ui, "Touch");
    row(ui, "Drag", "Turn the view");
    row(ui, "Pinch", "Zoom");
    row(
        ui,
        "Two fingers",
        "Drag to pan, twist to rotate, up/down to tilt",
    );
    row(ui, "Long press", "The same menu a right-click opens");
}

/// A line under a control saying what it does *not* do yet. Deliberately plain
/// text in the dim tone rather than a warning colour: it is a fact about the
/// build, not something the user did wrong.
fn pending(ui: &mut egui::Ui, text: &str) {
    theme::note(ui, text);
}

/// Where a person reaches the people who run this world.
///
/// **Published contact information is a store requirement, not a courtesy**
/// (App Store guideline 1.2, alongside the filter, the report path and the
/// block). It is a constant in one place because it is also printed in the About
/// panel and quoted in `docs/notes/user-safety.md`; three copies would go stale
/// separately, and the one that matters is the one a reviewer reads.
///
/// **This address must exist and be answered before a store submission.** An
/// unrouted mailbox is worse than none: it satisfies the letter of the guideline
/// and fails the person using it.
pub const SAFETY_CONTACT: &str = "safety@cartopolis.org";

/// The safety summary shown in About and in the Account tab.
///
/// Both places, deliberately. About is where a reviewer looks; the Account tab is
/// where a person who has just been harassed looks.
fn safety_section(ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("Safety").strong().color(theme::CREAM));
    ui.add(
        egui::Label::new(
            egui::RichText::new(
                "Tap or right-click somebody's name in chat to block or report them. \
                 Blocking is immediate and lasts until you undo it; reports go to the \
                 people who run this world.",
            )
            .size(theme::SIZE_SUB)
            .color(theme::TEXT_DIM),
        )
        .wrap(),
    );
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new("Contact:")
                .size(theme::SIZE_SUB)
                .color(theme::TEXT_DIM),
        );
        ui.label(
            egui::RichText::new(SAFETY_CONTACT)
                .size(theme::SIZE_SUB)
                .color(theme::ACCENT_TEXT),
        );
    });
}

/// Account: who you are signed in as, the safety summary, and the way out.
///
/// Returns `true` when a confirmed deletion should be sent.
///
/// **The deletion control is here because a store requires it to be** — an app
/// that supports account creation must offer deletion from inside the app
/// (guideline 5.1.1(v)), and until now the only route was the operator's web
/// admin GUI, which is not "within the app" by any reading.
fn account_tab(
    ui: &mut egui::Ui,
    state: &mut SettingsState,
    identity: &mut crate::resources::AuthIdentity,
    online: bool,
) -> bool {
    if identity.deleted {
        ui.label(
            egui::RichText::new("Your account has been deleted.")
                .strong()
                .color(theme::CREAM),
        );
        theme::note(
            ui,
            "Nothing of it remains on the server except shared chat history, which \
             no longer carries your name.",
        );
        return false;
    }

    match identity.username.clone() {
        Some(name) => {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new("Signed in as")
                        .size(theme::SIZE_SUB)
                        .color(theme::TEXT_DIM),
                );
                ui.label(egui::RichText::new(name).strong().color(theme::CREAM));
            });
        }
        None => {
            theme::note(
                ui,
                "You are not signed in. Offline, there is no account and nothing \
                 leaves this device.",
            );
        }
    }

    ui.add_space(8.0);
    ui.separator();
    safety_section(ui);
    ui.separator();
    ui.add_space(8.0);

    ui.label(
        egui::RichText::new("Delete this account")
            .strong()
            .color(theme::CREAM),
    );
    ui.add(
        egui::Label::new(
            egui::RichText::new(
                "This cannot be undone. The account, its password and its block list \
                 are erased; anything you said in Global or in a room stays, with your \
                 name removed from it.",
            )
            .size(theme::SIZE_SUB)
            .color(theme::TEXT_DIM),
        )
        .wrap(),
    );

    // Signed in, over a live session: the request travels on the wire, so there
    // is nothing to send while offline.
    let can_delete = online && identity.username.is_some();
    if !can_delete {
        theme::note(ui, "Sign in to the world holding the account to delete it.");
        return false;
    }

    if let Some(error) = identity.delete_error.clone() {
        if theme::inline_error(ui, &error) {
            identity.delete_error = None;
        }
    }

    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("Type your password to confirm")
            .size(theme::SIZE_SUB)
            .color(theme::TEXT_DIM),
    );
    theme::password_field(ui, &mut state.delete_password);
    let ready = !state.delete_password.is_empty();
    ui.add_enabled_ui(ready, |ui| {
        // Armed twice over: the password, and then a confirm that disarms itself
        // after a few seconds. This is the one control in the client that
        // destroys something nobody can restore.
        theme::confirm_button(
            ui,
            egui::Id::new("delete_account_confirm"),
            "Delete account…",
            "Delete permanently",
        )
    })
    .inner
        && ready
}

/// General: the settings a phone user looks for first.
fn general_tab(ui: &mut egui::Ui, prefs: &mut AppPrefs) {
    section(ui, "Units");
    ui.horizontal_wrapped(|ui| {
        for units in Units::ALL {
            ui.selectable_value(&mut prefs.units, units, units.label());
        }
    });
    // The consumers are `navigation::format_distance` / `format_duration` and the
    // driver bar's speed pill, all of which are metric-hardcoded today. The field
    // is here so there is one place for them to read, not so the switch does
    // something now.
    pending(
        ui,
        "Distances are shown in kilometres until the route and driver readouts read this.",
    );

    ui.add_space(8.0);
    ui.separator();
    section(ui, "Navigation");
    theme::checkbox(
        ui,
        &mut prefs.voice_guidance,
        "Chime at turns while navigating",
    );
    theme::checkbox(
        ui,
        &mut prefs.keep_screen_on,
        "Keep the screen on while driving",
    );
    // No wake-lock seam exists in `platform` — `navigator.wakeLock` on the web,
    // `FLAG_KEEP_SCREEN_ON` on Android — so this one is a stored intention.
    pending(
        ui,
        "The screen lock is the system's until this build can ask it to wait.",
    );
}

/// Map: how the scene is lit, as distinct from what the machine spends drawing
/// it — the detail preset and the render scale moved to [`graphics_tab`], where
/// they sit beside the other cost controls instead of under "Map".
fn map_tab(ui: &mut egui::Ui, prefs: &mut AppPrefs, sun_mode: &mut SunMode) {
    section(ui, "Lighting");
    ui.horizontal_wrapped(|ui| {
        // `Manual` is deliberately absent: it is the developer panel's mode (its
        // slider and Quick-cycle drop into it), and offering it here would be a
        // mode with no controls attached.
        for (mode, label) in [
            (SunMode::Auto, "Automatic"),
            (SunMode::Day, "Day"),
            (SunMode::Night, "Night"),
        ] {
            ui.selectable_value(sun_mode, mode, label);
        }
    });
    pending(
        ui,
        match sun_mode {
            SunMode::Auto => "The sun and moon follow the real time where the map is anchored.",
            SunMode::Day => "The sun is held at noon.",
            SunMode::Night => "The sun is held at midnight.",
            // Reachable: the dev panel's slider drops the clock into Manual, and
            // the mode is persisted, so it can be the state this panel opens in.
            SunMode::Manual => "The developer panel owns the clock — pick a mode to take it back.",
        },
    );
    let _ = prefs;
}

/// Graphics: what this machine spends on the picture.
///
/// **Every control here was in the developer panel**, which was wrong twice over.
/// It is reached by a keyboard chord this client's main platform does not have,
/// so the people who most need to turn something off — a phone that thermally
/// throttles, a driver that will not draw the sky — were the ones who could not;
/// and `DevPanelState` is not persisted, so a toggle labelled "restart" reverted
/// on the very restart that was supposed to apply it.
///
/// The tab is ordered by what a struggling device should reach for first: the
/// detail preset, then the render scale, then the individual features. Each
/// control says whether it applies now or at the next start, because the split is
/// real and not a UI convention — `quality::budget()` is a `OnceLock` read before
/// the world exists, and the sky and env map cannot be added to or removed from a
/// live camera without tearing down GPU resources still in flight.
fn graphics_tab(
    ui: &mut egui::Ui,
    prefs: &mut AppPrefs,
    target: Option<UVec2>,
    sky: Option<crate::systems::sky_dome::SkyMode>,
) {
    section(ui, "Detail");
    ui.horizontal_wrapped(|ui| {
        for preset in QualityPreset::ALL {
            ui.selectable_value(&mut prefs.quality, preset, preset.label());
        }
    });
    // `quality::budget()` is a process-wide `OnceLock`, read by systems that run
    // before the world exists (the worker pool sizes itself from it), so there is
    // no honest way to apply this to a running process.
    pending(
        ui,
        "Takes effect the next time the application starts. Automatic picks by device.",
    );

    ui.add_space(8.0);
    ui.separator();
    render_scale_row(ui, prefs, target);

    ui.add_space(8.0);
    ui.separator();
    sky_section(ui, prefs, sky);

    ui.add_space(8.0);
    ui.separator();
    section(ui, "Lighting");
    // A startup switch beats the preference at the spawn site, so the row has to
    // say so. Without this the checkbox reads "on" over a scene with no shadows
    // in it — which is precisely the failure the old dev-panel seeding was
    // written to avoid, reintroduced one layer up. Disabled rather than hidden:
    // the point is that the setting still exists and something else is holding
    // it.
    let shadows_forced_off = crate::env_flag("CARTO_NO_SHADOWS");
    ui.add_enabled_ui(!shadows_forced_off, |ui| {
        tri_state(
            ui,
            &mut prefs.shadows,
            crate::quality::budget().shadows,
            "Shadows",
        );
    });
    ui.label(
        egui::RichText::new(if shadows_forced_off {
            "Forced off for this run by CARTO_NO_SHADOWS."
        } else {
            "Sun and moon shadow maps. Applies immediately — the lights are respawned."
        })
        .small()
        .color(theme::TEXT_DIM),
    );

    ui.add_space(6.0);
    // Two things can hold this one down, so they are reported separately rather
    // than collapsed into one disabled state with a single reason: the cubemap is
    // baked *from* the atmosphere, so a `Simple` run has nothing to bake, and the
    // startup switch overrides the preference at the same site the shadow one
    // does. A control that vanishes reads as a bug, so both arms disable and
    // explain instead.
    let ibl_forced_off = crate::env_flag("CARTO_NO_ENVMAP");
    let ibl_possible = sky != Some(crate::systems::sky_dome::SkyMode::Dome) && !ibl_forced_off;
    ui.add_enabled_ui(ibl_possible, |ui| {
        tri_state(
            ui,
            &mut prefs.sky_lighting,
            crate::quality::budget().sky_ibl,
            "Sky reflections",
        );
    });
    ui.label(
        egui::RichText::new(if ibl_forced_off {
            "Forced off for this run by CARTO_NO_ENVMAP."
        } else if ibl_possible {
            "Lights shadowed faces with the sky and gives water something to \
             reflect. Costs a cubemap re-baked every frame. Takes effect at the \
             next start."
        } else {
            "Needs the full atmosphere — the simple sky has nothing to reflect."
        })
        .small()
        .color(theme::TEXT_DIM),
    );

    ui.add_space(8.0);
    ui.separator();
    section(ui, "Diagnostics");
    theme::checkbox(ui, &mut prefs.show_stats, "Performance overlay");
    ui.label(
        egui::RichText::new(
            "Frame rate, frame time and entity count, over the top-left of the map.",
        )
        .small()
        .color(theme::TEXT_DIM),
    );
}

/// The sky picker, plus what the run actually got.
///
/// The readout is the point, not decoration. A preference of "Automatic" and a
/// black sky is precisely the report this tab was built for (an Adreno 840,
/// 2026-08-19), and the one thing the person holding it could not find out was
/// which sky the run had chosen. Saying it here — beside the choice, in the panel
/// they are already in — is what turns "it's broken" into "it picked the full
/// atmosphere and the full atmosphere is what's wrong".
fn sky_section(
    ui: &mut egui::Ui,
    prefs: &mut AppPrefs,
    sky: Option<crate::systems::sky_dome::SkyMode>,
) {
    use crate::resources::SkyPreference;
    use crate::systems::sky_dome::SkyMode;

    section(ui, "Sky");
    ui.horizontal_wrapped(|ui| {
        for preference in SkyPreference::ALL {
            ui.selectable_value(&mut prefs.sky, preference, preference.label());
        }
    });
    pending(
        ui,
        "Takes effect the next time the application starts. Simple is cheaper and \
         works on every device.",
    );

    // What this run is drawing, and — when it differs from the request — why. The
    // capability is the only reason the answer can be narrower than the ask.
    let filterable = crate::quality::capabilities().map(|c| c.float32_filterable);
    let now = match sky {
        Some(SkyMode::Atmosphere) => "Drawing the full atmosphere.".to_string(),
        Some(SkyMode::Dome) => {
            let why = match (prefs.sky, filterable) {
                (SkyPreference::Simple, _) => "as asked",
                (_, Some(false)) => "this GPU cannot sample the atmosphere's lookup textures",
                _ => "forced by a startup switch",
            };
            format!("Drawing the simple sky — {why}.")
        }
        Some(SkyMode::None) => {
            "No sky at all — CARTO_NO_ATMOSPHERE is set for this run.".to_string()
        }
        None => "The renderer has not chosen a sky yet.".to_string(),
    };
    ui.label(egui::RichText::new(now).small().color(theme::TEXT_DIM));
}

/// A checkbox over an `Option<bool>`, where `None` means "follow this device".
///
/// The tri-state is not a flourish: writing a plain `bool` on first launch would
/// freeze *this* device's tier answer into `user.json` forever, so a later tier
/// change — or the same file carried to another machine — would be silently
/// overridden by a preference the user never expressed. So the checkbox shows the
/// resolved value, and an "Automatic" button appears once it has been overridden,
/// exactly as the render-scale row does.
fn tri_state(ui: &mut egui::Ui, value: &mut Option<bool>, tier_default: bool, label: &str) {
    let mut resolved = value.unwrap_or(tier_default);
    ui.horizontal(|ui| {
        if theme::checkbox(ui, &mut resolved, label).changed() {
            *value = Some(resolved);
        }
        if value.is_some() && theme::action_button(ui, "Automatic").clicked() {
            *value = None;
        }
    });
    if value.is_none() {
        ui.label(
            egui::RichText::new(if tier_default {
                "Automatic — on for this device."
            } else {
                "Automatic — off for this device."
            })
            .small()
            .color(theme::TEXT_DIM),
        );
    }
}

/// The render-scale slider.
///
/// Deliberately **not** followed by a `pending(...)` note, unlike the quality
/// preset directly above it, and the contrast is the point rather than an
/// oversight: that preset cannot apply to a running process (`quality::budget()`
/// is a `OnceLock` read before the world exists), while this is a `Resource` the
/// render world re-extracts every frame. Do not paste that note down here.
fn render_scale_row(ui: &mut egui::Ui, prefs: &mut AppPrefs, target: Option<UVec2>) {
    use crate::systems::render_scale::RenderScale;

    // The tier's own value is what "Automatic" resolves to, and it is what the
    // slider must show while the preference is `None` — otherwise dragging away
    // from Automatic makes the picture jump before it moves.
    let tier_default = crate::quality::budget().render_scale;
    let mut scale = prefs.render_scale.unwrap_or(tier_default);
    let auto = prefs.render_scale.is_none();

    ui.horizontal(|ui| {
        ui.label("Render scale");
        if !auto && theme::action_button(ui, "Automatic").clicked() {
            prefs.render_scale = None;
        }
    });

    let response = ui.add(
        egui::Slider::new(&mut scale, RenderScale::RANGE)
            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
            .step_by(0.05),
    );
    if response.changed() {
        prefs.render_scale = Some(RenderScale::new(scale).0);
    }

    // The resolved pixel count, not just the percentage. On a phone "50%" means
    // nothing and "540 x 1170" means everything, and it is the number a bug report
    // needs — the same reasoning as `build_line()` further down this panel. Uses
    // the renderer's own `scaled_size`, not a second copy of the arithmetic, so
    // the figure here cannot drift from the one the main pass uses.
    let hint = match target {
        Some(target) => {
            let scaled = crate::systems::render_scale::scaled_size(target, scale);
            format!(
                "The 3D world is drawn at {} x {} and scaled up.",
                scaled.x, scaled.y
            )
        }
        None => "The 3D world is drawn below the screen's resolution and scaled up.".to_string(),
    };
    ui.label(
        egui::RichText::new(if auto {
            format!("Automatic — following this device's tier. {hint}")
        } else {
            hint
        })
        .small()
        .color(theme::TEXT_DIM),
    );
    ui.label(
        egui::RichText::new("The interface stays sharp; only the map softens.")
            .small()
            .color(theme::TEXT_DIM),
    );
}

/// The Audio tab body. Returns `true` if the sound test was pressed this frame.
fn audio_tab(ui: &mut egui::Ui, state: &mut SettingsState, prefs: &mut AppPrefs) -> bool {
    let mut play_tone = false;

    section(ui, "Output");
    ui.label("Play a short chime to confirm you can hear the application.");
    // Disabled, not merely accompanied by a message: the button used to render
    // live and do nothing at all in a build with no audio backend, which reads as
    // a broken speaker rather than as a build without one.
    if ui
        .add_enabled(
            cfg!(feature = "audio"),
            egui::Button::new(format!("{}  Sound test", icons::PLAY)),
        )
        .clicked()
    {
        play_tone = true;
    }
    if !cfg!(feature = "audio") {
        ui.colored_label(theme::ERROR, "This build has no audio support.");
    }

    ui.add_space(10.0);
    ui.separator();
    section(ui, "Levels");
    // The clips are authored at the loudness they are meant to have — the
    // footsteps and the wind were re-cut in 2026-08-16 after a phone reported
    // them as a wind tunnel — so these start at unity and only ever come down.
    // What they are for is the half nobody else can judge: a specific speaker, in
    // a specific car, over a specific amount of road noise.
    ui.add(
        egui::Slider::new(&mut prefs.effects_volume, 0.0..=1.0)
            .text("Sound effects")
            .show_value(false),
    );
    ui.label(
        egui::RichText::new("Footsteps, wind, landings and the driving turn cues.")
            .small()
            .color(theme::TEXT_DIM),
    );
    ui.add(
        egui::Slider::new(&mut prefs.voice_volume, 0.0..=1.0)
            .text("Voice")
            .show_value(false),
    );
    ui.label(
        egui::RichText::new("How loudly people nearby are heard, before distance is applied.")
            .small()
            .color(theme::TEXT_DIM),
    );

    ui.add_space(10.0);
    ui.separator();
    section(ui, "Microphone");

    if !AudioInput::available() {
        // The `platform::location` contract: say unavailable rather than show a
        // meter that can never move.
        ui.colored_label(theme::ERROR, unavailable_reason());
        return play_tone;
    }

    // Above the test, because the test is how it is judged: the meter below shows
    // the *gained* signal, so moving this and watching the bar is the whole
    // calibration procedure.
    ui.add(
        egui::Slider::new(&mut prefs.mic_gain, 0.0..=crate::resources::MAX_MIC_GAIN)
            .text("Input gain")
            .show_value(false),
    );
    ui.label(
        egui::RichText::new("Lift a quiet headset. Aim for the middle of the bar while speaking.")
            .small()
            .color(theme::TEXT_DIM),
    );

    let running = state.mic.is_some();
    ui.horizontal(|ui| {
        if ui
            .button(if running {
                format!("{}  Stop", icons::STOP)
            } else {
                format!("{}  Test microphone", icons::MIC)
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

    if let Some((meter, peak)) = observe_peak(state, prefs.capture_gain()) {
        let mic = state.mic.as_ref().expect("observe_peak proved it is Some");
        if let Some(err) = mic.error() {
            ui.colored_label(theme::ERROR, err);
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
///
/// The Android arm this used to lead with is gone: the phone has a capture
/// backend now (`platform::audio`'s cpal arm, over AAudio), so what a phone
/// reaches here is the *feature* answer or nothing at all. The lesson that put
/// the platform test first survives in its absence — a message that sends
/// someone to fix the wrong thing is worse than the generic one it replaced, and
/// a permission refusal is not this function's business: it arrives as
/// `AudioInput::error`, beside the meter, where the person can act on it.
fn unavailable_reason() -> &'static str {
    if !cfg!(feature = "audio") {
        "This build has no audio support (built without the `audio` feature)."
    } else {
        "No microphone backend on this platform."
    }
}

/// A level meter with a peak-hold tick.
///
/// Shared with the chat panel's voice row rather than redrawn there: the two
/// answer the same question about the same device, and a second bar with its own
/// colours is how the chrome drifts (`systems::theme`'s note).
pub(crate) fn level_bar(ui: &mut egui::Ui, meter: f32, peak: f32) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 14.0), egui::Sense::hover());
    let p = ui.painter();
    let radius = egui::CornerRadius::same(theme::RADIUS_SM);
    p.rect_filled(rect, radius, theme::SURFACE);

    if meter > 0.0 {
        let filled =
            egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * meter, rect.height()));
        p.rect_filled(filled, radius, meter_colour(meter));
    }
    if peak > 0.0 {
        let x = rect.min.x + rect.width() * peak;
        p.line_segment(
            [egui::pos2(x, rect.min.y), egui::pos2(x, rect.max.y)],
            egui::Stroke::new(1.5, theme::CREAM),
        );
    }
}

/// Green through amber to the brand orange as the signal approaches clipping.
///
/// The top of the bar is a warning, not a target — a meter that is green at
/// full scale invites people to clip their own microphone.
fn meter_colour(meter: f32) -> egui::Color32 {
    if meter > 0.9 {
        theme::ERROR
    } else if meter > 0.75 {
        theme::AMBER
    } else {
        theme::ACCENT
    }
}

// ─── About ────────────────────────────────────────────────────────────────────

/// The canonical third-party notices, embedded at compile time.
///
/// `include_str!` rather than a runtime read: the file has to be *in* the binary
/// on Android and on the web, where there is no working directory to find it
/// from, and a licence notice that is missing on exactly the platforms that ship
/// as a single artefact is the one case that matters.
const THIRD_PARTY: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../LICENSE-THIRD-PARTY.md"
));

/// One record from [`THIRD_PARTY`] — see that file's own `## Format` section,
/// which this parser implements.
#[derive(Debug, PartialEq, Eq)]
pub struct Notice {
    pub name: String,
    pub role: String,
    pub license: String,
    pub url: String,
    /// The exact string a UI must show, when the licence demands one.
    pub notice: String,
    /// A limit worth stating — several sources are Netherlands-only.
    pub scope: String,
}

/// Parse the notices file into records.
///
/// Deliberately forgiving in one direction and strict in the other: prose before
/// the first `## ` is skipped, and an unknown key is ignored (the file is edited
/// by hand and may grow a key before this does) — but a record with no `notice`
/// keeps its empty string rather than being dropped, because a source that is
/// listed and unrendered is precisely the failure this panel exists to prevent.
pub fn parse_notices(text: &str) -> Vec<Notice> {
    let mut out: Vec<Notice> = Vec::new();
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("## ") {
            out.push(Notice {
                name: name.trim().to_string(),
                role: String::new(),
                license: String::new(),
                url: String::new(),
                notice: String::new(),
                scope: String::new(),
            });
            continue;
        }
        let Some(record) = out.last_mut() else {
            continue; // still in the prose header
        };
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        // Keys are lowercase ASCII with no leading space; anything else is a
        // sentence that happens to contain a colon.
        let value = value.trim().to_string();
        match key {
            "role" => record.role = value,
            "license" => record.license = value,
            "url" => record.url = value,
            "notice" => record.notice = value,
            "scope" => record.scope = value,
            _ => {}
        }
    }
    // A heading with no `license:` and no `notice:` under it is prose, not a
    // record — the file's own `## Format` section is one, and rendering it would
    // put an empty entry called "Format" at the top of the credits. Keyed on the
    // two fields that make a record a *notice*, so a source that loses its url or
    // its role is still shown.
    out.retain(|n| !n.license.is_empty() || !n.notice.is_empty());
    out
}

/// The About panel: what this build is, and everything it is built from.
///
/// In the Phase 7b `.chain()` with the other panels — it draws into the shared
/// root `Ui`, so leaving it out is the dead-click failure that note describes.
pub fn render_about_panel(
    mut egui: ResMut<EguiManager>,
    mut right_panel: ResMut<RightPanel>,
    mut notices: Local<Option<Vec<Notice>>>,
) {
    // Called unconditionally, open or not: `show_collapsible` drives the
    // slide-out animation and keeps the shared root `Ui`'s auto-id stream the
    // same either way (`skip_ahead_auto_ids`), so an early return on a closed
    // panel shifts every id after it in the chain.
    //
    // Parsed once per process, not once per frame: it is ~130 lines of static
    // text and the answer cannot change while the binary runs.
    let notices = notices.get_or_insert_with(|| parse_notices(THIRD_PARTY));

    crate::systems::egui_manager::right_panel(
        &mut egui,
        &mut right_panel,
        RightPanel::About,
        "about_panel",
        320.0,
        |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new("Cartopolis")
                            .font(egui::FontId::new(
                                theme::SIZE_HEADING,
                                theme::brand_family(),
                            ))
                            .color(theme::CREAM),
                    );
                    ui.label(
                        egui::RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                            .color(theme::TEXT),
                    );
                    ui.label(
                        egui::RichText::new(build_line())
                            .size(theme::SIZE_CHIP)
                            .color(theme::TEXT_DIM),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(
                            "Cartopolis's own source is MIT OR Apache-2.0. \
                         Everything below is used under its own licence.",
                        )
                        .size(theme::SIZE_CHIP)
                        .color(theme::TEXT_DIM),
                    );
                    ui.add_space(6.0);
                    ui.separator();
                    // Above the credits rather than below them: this is the one
                    // block on the panel somebody may be looking for in a hurry,
                    // and the licence list is ~130 lines of scrolling.
                    safety_section(ui);
                    ui.separator();
                    controls_section(ui);
                    ui.separator();

                    for record in notices.iter() {
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new(&record.name)
                                .strong()
                                .color(theme::CREAM),
                        );
                        if !record.role.is_empty() {
                            ui.label(egui::RichText::new(&record.role).size(theme::SIZE_CHIP));
                        }
                        // The `notice` line is the one the licence actually requires,
                        // so it is the one shown at body weight.
                        if !record.notice.is_empty() {
                            ui.label(egui::RichText::new(&record.notice).size(theme::SIZE_SUB));
                        }
                        ui.horizontal_wrapped(|ui| {
                            if !record.license.is_empty() {
                                ui.label(
                                    egui::RichText::new(&record.license)
                                        .size(theme::SIZE_CHIP)
                                        .color(theme::ACCENT_TEXT),
                                );
                            }
                            if !record.scope.is_empty() {
                                ui.label(
                                    egui::RichText::new(format!("· {}", record.scope))
                                        .size(theme::SIZE_CHIP)
                                        .color(theme::TEXT_DIM),
                                );
                            }
                        });
                        if !record.url.is_empty() {
                            // Disabled where nothing can open it, rather than dead —
                            // the same contract the POI popup's `Website ↗` follows.
                            let can_open = crate::systems::data_layers::open_url_supported();
                            let button = ui.add_enabled(
                                can_open,
                                egui::Button::new(
                                    egui::RichText::new(&record.url)
                                        .size(theme::SIZE_CHIP)
                                        .color(theme::ACCENT_TEXT),
                                )
                                .frame(false),
                            );
                            if button.clicked() {
                                crate::systems::data_layers::open_url(&record.url);
                            }
                        }
                        ui.add_space(2.0);
                        ui.separator();
                    }
                });
        },
    );
}

/// What this binary is, in one line: the platform it was built for, the profile,
/// and the render tier it resolved at startup. All three are the first questions
/// asked of a bug report, and the phone is the one device with no console to ask
/// them from.
fn build_line() -> String {
    format!(
        "{} · {} · {} · {:?} tier",
        std::env::consts::OS,
        std::env::consts::ARCH,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        crate::quality::budget().tier,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_holds_the_maximum_and_resets_with_the_test() {
        let mut state = SettingsState::default();
        // No mic: nothing to observe, and no panic.
        assert!(observe_peak(&mut state, 1.0).is_none());

        state.peak = 0.6;
        state.stop_mic();
        assert_eq!(state.peak, 0.0, "stopping a test clears the peak hold");
    }

    #[test]
    fn meter_warns_before_it_reaches_the_end() {
        // The bar must change colour below full scale, or it reads as a target.
        assert_eq!(meter_colour(0.5), theme::ACCENT);
        assert_eq!(meter_colour(0.8), theme::AMBER);
        assert_eq!(meter_colour(0.95), theme::ERROR);
    }

    /// The Android arm has to be *reachable*, which is the whole bug: the APK is
    /// built `--no-default-features`, so the build-flag test is true there too,
    /// and whichever branch comes first is the message every Android user gets.
    #[test]
    fn unavailable_reason_blames_the_platform_before_the_build_flag() {
        let reason = unavailable_reason();
        assert!(reason.ends_with('.'), "reason should read as a sentence");
        if cfg!(target_os = "android") {
            assert!(
                reason.contains("Android"),
                "on Android the answer is the platform, not a cargo feature"
            );
        } else if !cfg!(feature = "audio") {
            assert!(reason.contains("audio"), "should name the missing feature");
        }
    }

    /// Every tab must be reachable from the strip, and the strip is built from
    /// `ALL` — a variant missing from it is a tab that exists and cannot be
    /// opened.
    #[test]
    fn every_tab_is_in_the_tab_strip() {
        // Named individually rather than iterated, because `ALL` is exactly what
        // is under test: a variant added to the enum but forgotten here is a tab
        // that exists, compiles, has a label and a body, and can never be opened.
        // `label()`'s `match` is exhaustive, so the compiler covers the naming;
        // nothing but this list covers the reachability.
        for tab in [
            SettingsTab::General,
            SettingsTab::Map,
            SettingsTab::Graphics,
            SettingsTab::Audio,
            SettingsTab::Account,
        ] {
            assert!(SettingsTab::ALL.contains(&tab), "{tab:?} is not in ALL");
            assert!(!tab.label().is_empty());
        }
    }

    /// Two tabs sharing a label would make one of them unreachable in the strip
    /// while both still rendered — the sort of thing a copy-pasted arm produces.
    #[test]
    fn tab_labels_are_distinct() {
        let mut seen: Vec<&str> = SettingsTab::ALL.iter().map(|t| t.label()).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), before, "two tabs share a label");
    }

    /// The sky picker's rungs, same contract as the tab strip: a preference the
    /// enum has and `ALL` does not is one nobody can choose.
    #[test]
    fn every_sky_preference_is_offered() {
        use crate::resources::SkyPreference;
        for preference in [
            SkyPreference::Auto,
            SkyPreference::Full,
            SkyPreference::Simple,
        ] {
            assert!(
                SkyPreference::ALL.contains(&preference),
                "{preference:?} is not in ALL"
            );
            assert!(!preference.label().is_empty());
        }
    }

    /// The notices file is data, and this is the parser for its documented
    /// format. Prose before the first heading is skipped; `key: value` lines fill
    /// the record they follow.
    #[test]
    fn the_notices_format_parses() {
        let text = "\
Some prose about the file.
It even has a colon: like this.

## Bevy

role: Game engine
license: MIT OR Apache-2.0
url: https://bevy.org/
notice: Built with Bevy.

## 3DBAG

role: LoD2.2 building models
license: CC BY 4.0
scope: Netherlands only
";
        let notices = parse_notices(text);
        assert_eq!(notices.len(), 2, "prose must not open a record");
        assert_eq!(notices[0].name, "Bevy");
        assert_eq!(notices[0].notice, "Built with Bevy.");
        assert_eq!(notices[0].url, "https://bevy.org/");
        assert_eq!(notices[1].name, "3DBAG");
        assert_eq!(notices[1].scope, "Netherlands only");
        // A record with no `notice:` but a licence is kept, not dropped: an
        // unrendered source is exactly the failure this panel exists to prevent.
        assert!(notices[1].notice.is_empty());
    }

    /// The shipped file opens with a `## Format` section describing itself. It
    /// is prose under a heading, and a parser that took it for a record would
    /// put an empty credit called "Format" at the top of the panel.
    #[test]
    fn the_files_own_prose_sections_are_not_credits() {
        let notices = parse_notices(THIRD_PARTY);
        assert!(
            !notices.iter().any(|n| n.name == "Format"),
            "the format description parsed as a credit"
        );
        for record in &notices {
            assert!(
                !record.license.is_empty() || !record.notice.is_empty(),
                "{} has nothing to say",
                record.name
            );
        }
    }

    /// The four attributions that are a legal obligation rather than a courtesy
    /// must be in the shipped file, and must arrive with the text the licence
    /// asks for. This is the test that fails if someone trims the file.
    #[test]
    fn the_required_attributions_are_present_and_rendered() {
        let notices = parse_notices(THIRD_PARTY);
        assert!(
            notices.len() >= 10,
            "the notices file parsed to {} records",
            notices.len()
        );
        for required in [
            "OpenStreetMap",
            "3DBAG",
            "Quaternius — Ultimate Modular Men / Women",
            "Plus Jakarta Sans",
        ] {
            let record = notices
                .iter()
                .find(|n| n.name == required)
                .unwrap_or_else(|| panic!("{required} is not in LICENSE-THIRD-PARTY.md"));
            assert!(
                !record.notice.is_empty(),
                "{required} has no notice string to render"
            );
            assert!(!record.license.is_empty(), "{required} has no licence");
        }
    }
}
