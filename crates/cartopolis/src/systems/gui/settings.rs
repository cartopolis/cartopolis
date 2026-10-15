//! The Settings panel — and the About panel, which is next to it because they
//! are the two screens that are *about the application* rather than about the
//! world.
//!
//! # Settings
//!
//! Five tabs: General, World, Graphics, Audio, Account. It began as one (Audio),
//! which made a panel called "Settings" that was really a hardware diagnostic —
//! the two things behind it were a chime and a level meter, and every actual
//! preference in the client lived somewhere else: the map layers in their own
//! panel, the sky in the HUD switcher and the dev panel, the render budget
//! nowhere at all.
//!
//! **Graphics and World are the two halves of the same migration**, and the split
//! between them is the line worth keeping. Shadows, the sky, sky reflections and
//! the stats overlay were `DevPanelState` fields, reached by a keyboard chord —
//! on a client whose main platform is a phone, which has no keyboard and no
//! developer panel worth opening. So the settings a struggling device most needs
//! were the ones its owner could not reach. They were also unpersisted, which
//! made every "restart to apply" toggle a lie: the restart that applied it also
//! discarded it. World is the rest of that panel's contents — the clock, the
//! moon, the weather — moved for the first of those two reasons alone, since
//! those are still stored in `DevPanelState` and still not persisted there.
//!
//! Graphics is **what this machine spends on the picture**; World is **what the
//! picture is of**. A new control belongs on whichever side of that answers it,
//! and a control that belongs on neither is probably a measurement, which is
//! what [`crate::systems::dev_panel::render_dev_panel`] kept.
//!
//! **Two things went back the other way in 2026-09-10**, and both are the same
//! test applied honestly. The performance overlay was a Graphics checkbox for a
//! box of frame rate and entity count over the map: not a setting, a readout,
//! and one the Developer tools' Frame tab already gave in more detail — deleted.
//! The `CARTO_*` bisecting switches were a collapsed section at the foot of this
//! tab whose own header had to explain that they are not quality knobs: several
//! of them make the picture worse on purpose, and a run with one on is a run
//! whose rendering proves nothing. A section that must apologise for the panel it
//! is in belongs in another panel, and Developer tools ▸ Switches is it. They
//! were put here in the first place for a good reason that is no longer load
//! bearing — a phone cannot press Ctrl+Shift+D — and that reason expired when
//! Developer tools got a main-menu entry.
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

use crate::systems::egui_manager::EguiMut;
use bevy::prelude::*;

use crate::platform::audio::{self, AudioInput};
use crate::resources::{AppPrefs, QualityPreset, RightPanel, SunMode, Units};
use crate::systems::cache_reset::CacheReset;
use crate::systems::icons;
use crate::systems::theme;

/// Which tab of the Settings panel is showing.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsTab {
    #[default]
    General,
    /// The world's clock and its weather — what the scene *is*. Called World and
    /// not Map because the map's layers have a panel of their own; this tab is
    /// the time of day, the season and the sky over it. See [`world_tab`].
    World,
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
        Self::World,
        Self::Graphics,
        Self::Audio,
        Self::Account,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::World => "World",
            Self::Graphics => "Graphics",
            // "Audio & video" until the tabs became one segmented row, where
            // five equal segments leave no room for three words.
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
    /// The meter as drawn: rises with the signal at once and falls back
    /// smoothly, as a hardware meter's needle does.
    pub shown: f32,
    /// The peak-hold mark: the highest recent level, held for a second, then
    /// falling.
    pub peak: f32,
    /// When the peak mark was last raised (`egui` seconds).
    pub peak_at: f64,
    /// The camera, while the camera test runs.
    #[cfg(not(target_arch = "wasm32"))]
    pub camera: Option<crate::platform::camera::Camera>,
    /// The camera test's picture on the GPU, replaced as frames arrive.
    pub camera_texture: Option<egui::TextureHandle>,
    /// A level to draw on the meter with no microphone running: a scripted
    /// capture's (`--ui audio`), which has no device to move it. (meter, peak)
    pub demo_level: Option<(f32, f32)>,
    /// The password re-entered to confirm account deletion.
    ///
    /// Cleared the moment the request goes out and whenever the panel closes: it
    /// is the one string in this struct that must not outlive the interaction
    /// that produced it.
    pub delete_password: String,
    /// The GNSS receiver's settings as being edited, applied with a button —
    /// a receiver restarted on every keystroke of a caster's host name would
    /// spend the typing reconnecting. `None` until the section is first drawn.
    #[cfg(not(target_arch = "wasm32"))]
    pub receiver_draft: Option<crate::systems::gnss_receiver::ReceiverPrefs>,
    /// The serial ports last listed, and when (`egui` seconds): listing reads
    /// `/sys`, which is not something to do sixty times a second. Desktop only
    /// — a phone's adapter is not chosen from a list.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    pub receiver_ports: Vec<(String, String)>,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    pub receiver_ports_at: Option<f64>,
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
        self.shown = 0.0;
    }

    /// Begin the camera test, replacing any running one.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn start_camera(&mut self) {
        self.camera_texture = None;
        self.camera = Some(if crate::env_flag("CARTO_FAKE_CAMERA") {
            // The stand-in camera, as a call uses under the same switch.
            crate::platform::camera::Camera::fake()
        } else {
            crate::platform::camera::Camera::open()
        });
    }

    /// Stop the camera test and close the device.
    pub fn stop_camera(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.camera = None;
        }
        self.camera_texture = None;
    }

    /// Whether the camera test is running.
    #[must_use]
    pub fn camera_running(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        return self.camera.is_some();
        #[cfg(target_arch = "wasm32")]
        false
    }
}

/// Track the peak while a test runs. Split out so the panel body stays layout.
///
/// `gain` is [`AppPrefs::capture_gain`], applied here because the meter is the
/// only place the input gain can be judged — voice transmits the gained signal,
/// so a meter showing the raw one would have someone calibrate against a number
/// nobody sends.
fn observe_peak(state: &mut SettingsState, gain: f32, now: f64, dt: f32) -> Option<(f32, f32)> {
    let mic = state.mic.as_ref()?;
    let meter = audio::level_to_meter((mic.level() * gain).clamp(0.0, 1.0));
    Some(meter_ballistics(state, meter, now, dt))
}

/// Meter ballistics: the bar jumps up with the signal and falls at
/// [`METER_FALL`] a second; the peak mark holds for [`PEAK_HOLD_S`], then falls
/// at [`PEAK_FALL`]. Without them a meter fed a raw level flickers from frame
/// to frame and the peak mark is either stuck or gone.
fn meter_ballistics(state: &mut SettingsState, meter: f32, now: f64, dt: f32) -> (f32, f32) {
    state.shown = meter.max(state.shown - METER_FALL * dt).max(0.0);
    if meter >= state.peak {
        state.peak = meter;
        state.peak_at = now;
    } else if now - state.peak_at > PEAK_HOLD_S {
        state.peak = (state.peak - PEAK_FALL * dt).max(state.shown);
    }
    (state.shown, state.peak)
}

/// How fast the bar falls, in full scales a second.
const METER_FALL: f32 = 1.6;
/// How long the peak mark holds before it falls, in seconds.
const PEAK_HOLD_S: f64 = 1.0;
/// How fast the peak mark falls once released, in full scales a second.
const PEAK_FALL: f32 = 0.6;

/// The Settings panel. Ordered in the panel `.chain()` like every other panel
/// — see the note there; an unordered panel loses egui's hit test at random.
#[allow(clippy::too_many_arguments)]
pub fn render_settings_panel(
    mut egui: EguiMut,
    mut right_panel: ResMut<RightPanel>,
    mut state: ResMut<SettingsState>,
    mut prefs: ResMut<AppPrefs>,
    mut dev: ResMut<crate::resources::DevPanelState>,
    // The World tab's Automatic/Manual switch *is* `WeatherState::live`, and the
    // readout under Automatic is its last sample. The sliders it unlocks live in
    // `DevPanelState` beside the clock, which is why both resources are here.
    mut weather: ResMut<crate::systems::weather::WeatherState>,
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
    // The Storage section draws the state of the last reset and writes the next
    // request into it. The work itself is `cache_reset::apply_cache_reset`'s —
    // emptying a store is a directory walk on another thread, which is not
    // something an egui pass can wait for.
    mut cache: ResMut<CacheReset>,
    // Where an editor attaches and what it must present — see
    // `editor_access_section`. One parameter rather than three, because this
    // system was already at thirteen of Bevy's sixteen.
    #[cfg(mcp)] mut mcp: crate::systems::mcp::EditorAccess,
    // The external GNSS receiver (`systems::gnss_receiver`). Edited through a
    // draft in `SettingsState` and written back only on Apply or the switch.
    mut receiver: ResMut<crate::systems::gnss_receiver::ReceiverPrefs>,
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
    let mut reset_cache = false;
    let mut editor = EditorRow::default();
    #[cfg(mcp)]
    {
        editor.serving = mcp.serving().map(|s| match s {
            crate::systems::mcp::Serving::At(addr) => EditorServing::At(addr),
            crate::systems::mcp::Serving::Off => EditorServing::Off,
            crate::systems::mcp::Serving::Failed(why) => EditorServing::Failed(why),
        });
        (editor.allow, editor.allow_pinned) = mcp.allow();
    }

    // Edited as plain values and written back only on a real change. All three of
    // these resources are read by `is_changed()` gates — `DevPanelState`'s re-runs
    // the whole day/night lighting recompute — and `ui.selectable_value(&mut
    // res.field, …)` takes a `DerefMut`, i.e. marks the resource changed on *every
    // frame the panel is open*. With a slider in the tab that is not a throttling
    // nicety: dragging the clock would otherwise recompute the lighting from a
    // resource that reports itself changed whether the drag moved anything or not.
    // This is the same shape `hud::render_status_hud` uses for the sun switcher
    // and for the same reason.
    let mut edited = *prefs;
    let mut world = dev.clone();
    let mut weather_live = weather.live;
    let observed = weather.latest;
    let mut receiver_edit = receiver.clone();

    crate::systems::egui_manager::right_panel(
        &mut egui,
        &mut right_panel,
        RightPanel::Settings,
        "settings_panel",
        340.0,
        |ui| {
            let tabs = SettingsTab::ALL.map(|t| (t, t.label()));
            crate::systems::theme::segmented(ui, &mut state.tab, &tabs);
            ui.add_space(6.0);

            // Every tab is a list that can outgrow a phone's 609 pt of viewport, and
            // a `Panel` clips rather than scrolls (the Layers panel had lost its last
            // ten rows to exactly this). The header stays outside, so the close
            // button cannot scroll away.
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match state.tab {
                    SettingsTab::General => {
                        reset_cache = general_tab(
                            ui,
                            &mut edited,
                            &cache,
                            &mut editor,
                            &mut state,
                            &mut receiver_edit,
                        );
                    }
                    SettingsTab::World => {
                        world_tab(ui, &mut world, &mut weather_live, observed);
                    }
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
    if world != *dev {
        *dev = world;
    }
    if weather_live != weather.live {
        weather.live = weather_live;
    }
    if receiver_edit != *receiver {
        *receiver = receiver_edit;
    }
    // Closing the panel must release the microphone: leaving it open behind a
    // hidden panel is the recording-indicator problem the module note describes.
    if !right_panel.is(RightPanel::Settings) && state.mic.is_some() {
        state.stop_mic();
    }
    // The camera likewise, and on leaving the tab too: a camera light that
    // stays on behind another screen is the same problem, more visible.
    if (!right_panel.is(RightPanel::Settings) || state.tab != SettingsTab::Audio)
        && state.camera_running()
    {
        state.stop_camera();
    }
    // …and it must drop the typed password, for the same shape of reason.
    if !right_panel.is(RightPanel::Settings) && !state.delete_password.is_empty() {
        state.delete_password.clear();
    }

    if reset_cache {
        cache.requested = true;
    }

    #[cfg(mcp)]
    if let Some(list) = editor.allow_edited.take() {
        mcp.set_allow(list);
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
///
/// Returns `true` when the Storage section's confirm was pressed, for
/// `systems::cache_reset::apply_cache_reset` to act on.
fn general_tab(
    ui: &mut egui::Ui,
    prefs: &mut AppPrefs,
    cache: &CacheReset,
    editor: &mut EditorRow,
    state: &mut SettingsState,
    receiver: &mut crate::systems::gnss_receiver::ReceiverPrefs,
) -> bool {
    section(ui, "Units");
    let units = Units::ALL.map(|u| (u, u.label()));
    crate::systems::theme::segmented(ui, &mut prefs.units, &units);
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

    // Android only, and **hidden rather than disabled** — the opposite call to
    // the one `world_tab` makes for its manual controls, for a reason. A disabled
    // control says "not now"; there is no "now" on the other two platforms. A
    // desktop holds its socket for as long as the process lives, so the setting
    // is already true there and switching it would change nothing; a wasm client
    // dies with its tab, so it would be a promise the build cannot keep.
    #[cfg(target_os = "android")]
    {
        ui.add_space(8.0);
        ui.separator();
        section(ui, "Connection");
        theme::checkbox(
            ui,
            &mut prefs.background_connection,
            "Stay connected in the background",
        );
        // Said plainly and before the fact, because both consequences are things
        // a person would otherwise discover in the notification shade and the
        // battery breakdown, and neither is undoable from there.
        pending(
            ui,
            "Adds a permanent notification while it is on, and keeps the app \
             running when you leave it. Only applies while signed in.",
        );
    }

    receiver_section(ui, state, receiver);

    ui.add_space(8.0);
    ui.separator();
    let reset = storage_section(ui, cache);
    editor_access_section(ui, prefs, editor);
    reset
}

/// Nothing can be plugged into a browser tab — see `platform::gnss_receiver`.
#[cfg(target_arch = "wasm32")]
fn receiver_section(
    _ui: &mut egui::Ui,
    _state: &mut SettingsState,
    _prefs: &mut crate::systems::gnss_receiver::ReceiverPrefs,
) {
}

/// An external GNSS receiver — an RTK rover on a cable — and the NTRIP caster
/// that feeds it corrections (`systems::gnss_receiver`,
/// `docs/notes/rtk-receiver.md`).
///
/// **The switch applies at once; everything under it waits for Apply.** The
/// switch is the one control whose effect a person expects immediately, and
/// the rest are text that is wrong until it is finished.
///
/// The readout under the switch is the receiver's own account of itself, and
/// it is the point of the section as much as the settings are: "connected" is
/// not "fixed", and the difference between RTK float and RTK fixed — or between
/// a caster streaming RTCM and one answering with a sourcetable — is invisible
/// anywhere else.
#[cfg(not(target_arch = "wasm32"))]
fn receiver_section(
    ui: &mut egui::Ui,
    state: &mut SettingsState,
    prefs: &mut crate::systems::gnss_receiver::ReceiverPrefs,
) {
    use crate::systems::gnss_receiver::{BAUD_RATES, RATES_HZ};

    ui.add_space(8.0);
    ui.separator();
    section(ui, "GNSS receiver");
    let draft = state.receiver_draft.get_or_insert_with(|| prefs.clone());
    let mut enabled = prefs.enabled;
    if theme::checkbox(ui, &mut enabled, "Use an external receiver for my position").changed() {
        prefs.enabled = enabled;
        draft.enabled = enabled;
    }
    if !prefs.enabled {
        theme::note(
            ui,
            "A GNSS receiver on a cable — an RTK rover such as the UM980 — gives this \
             device its position: metres on its own, centimetres with corrections from \
             an NTRIP caster.",
        );
        return;
    }
    if let Some(status) = crate::platform::gnss_receiver::status() {
        for (warn, line) in crate::systems::gnss_receiver::status_lines(&status) {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(line)
                        .size(theme::SIZE_SUB)
                        .color(if warn { theme::WARN } else { theme::TEXT }),
                )
                .wrap(),
            );
        }
    }
    // The receiver's own account — settings, state and event history — for
    // pasting into a bug report. The clipboard works on the phone too
    // (`platform::android::copy_text`), and this is what a phone's rolling
    // log cannot be relied on to still hold.
    if ui
        .button(format!("{}  Copy receiver report", icons::COPY))
        .clicked()
    {
        crate::utils::clipboard::set_text(&crate::systems::gnss_receiver::report(prefs));
    }
    ui.add_space(6.0);

    let label = |ui: &mut egui::Ui, text: &str| {
        ui.label(
            egui::RichText::new(text)
                .size(theme::SIZE_CHIP)
                .color(theme::TEXT_DIM),
        );
    };
    // The port is chosen on the desktop only: on a phone the first supported
    // USB adapter is the receiver, and there is nothing to list.
    #[cfg(not(target_os = "android"))]
    {
        let now = ui.input(|i| i.time);
        if state.receiver_ports_at.is_none_or(|t| now - t > 3.0) {
            state.receiver_ports = crate::platform::gnss_receiver::ports();
            state.receiver_ports_at = Some(now);
        }
        let draft = state.receiver_draft.as_mut().expect("drafted above");
        label(ui, "Port");
        let shown = if draft.port.is_empty() {
            "Automatic".to_string()
        } else {
            state
                .receiver_ports
                .iter()
                .find(|(p, _)| *p == draft.port)
                .map_or_else(|| draft.port.clone(), |(_, l)| l.clone())
        };
        egui::ComboBox::from_id_salt("gnss_port")
            .selected_text(shown)
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut draft.port, String::new(), "Automatic");
                for (path, l) in &state.receiver_ports {
                    ui.selectable_value(&mut draft.port, path.clone(), l);
                }
            });
    }
    let draft = state.receiver_draft.as_mut().expect("drafted above");
    ui.horizontal(|ui| {
        label(ui, "Baud");
        egui::ComboBox::from_id_salt("gnss_baud")
            .selected_text(draft.baud.to_string())
            .show_ui(ui, |ui| {
                for b in BAUD_RATES {
                    ui.selectable_value(&mut draft.baud, b, b.to_string());
                }
            });
        label(ui, "Rate");
        egui::ComboBox::from_id_salt("gnss_rate")
            .selected_text(format!("{} Hz", draft.rate_hz))
            .show_ui(ui, |ui| {
                for r in RATES_HZ {
                    ui.selectable_value(&mut draft.rate_hz, r, format!("{r} Hz"));
                }
            });
    });
    let carried = cartopolis_gnss::receiver::rate_for_baud(draft.rate_hz, draft.baud);
    if carried < draft.rate_hz {
        theme::note(
            ui,
            &format!(
                "{} baud carries {carried} Hz; the receiver will be asked for that.",
                draft.baud
            ),
        );
    }
    theme::checkbox(
        ui,
        &mut draft.configure,
        "Set the receiver's output (Unicore receivers)",
    );
    theme::note(
        ui,
        "Asks for position, speed and accuracy at the rate above on this port. Not \
         saved in the receiver: unplugged, it goes back to its own settings.",
    );

    ui.add_space(6.0);
    theme::checkbox(
        ui,
        &mut draft.ntrip.enabled,
        "RTK corrections from an NTRIP caster",
    );
    if draft.ntrip.enabled {
        let n = &mut draft.ntrip;
        let field = |ui: &mut egui::Ui, id: &str, text: &mut String, hint: &str, secret: bool| {
            ui.add(
                egui::TextEdit::singleline(text)
                    .id(egui::Id::new(id))
                    .hint_text(hint)
                    .password(secret)
                    .desired_width(f32::INFINITY),
            );
        };
        label(ui, "Caster");
        // The Netherlands' national reference stations, free and without an
        // account: the one choice most people here would otherwise have to
        // research, and whose port (443, TLS only) nobody would guess.
        if ui
            .button("Kadaster — free, Netherlands")
            .on_hover_text(crate::systems::gnss_receiver::KADASTER_HOST)
            .clicked()
        {
            n.use_kadaster();
        }
        ui.horizontal(|ui| {
            let mut port = n.port.to_string();
            ui.add(
                egui::TextEdit::singleline(&mut port)
                    .id(egui::Id::new("ntrip_port"))
                    .desired_width(56.0),
            );
            if let Ok(p) = port.trim().parse::<u16>() {
                n.port = p;
            }
            field(ui, "ntrip_host", &mut n.host, "caster host name", false);
        });
        let mut tls = n.tls || n.port == 443;
        if theme::checkbox(ui, &mut tls, "Encrypted connection (TLS)").changed() {
            n.tls = tls;
        }
        label(ui, "Mountpoint");
        field(
            ui,
            "ntrip_mount",
            &mut n.mountpoint,
            "empty: the nearest station",
            false,
        );
        label(ui, "Username");
        field(
            ui,
            "ntrip_user",
            &mut n.username,
            "if the caster asks for one",
            false,
        );
        label(ui, "Password");
        field(ui, "ntrip_password", &mut n.password, "", true);
        theme::checkbox(ui, &mut n.send_position, "Send my position to the caster");
        theme::note(
            ui,
            "Network (VRS) mountpoints need it to make corrections for where you are. \
             The caster's operator sees the position.",
        );
    }

    if *draft != *prefs {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("Apply").clicked() {
                *prefs = draft.clone();
            }
            if ui.button("Revert").clicked() {
                *draft = prefs.clone();
            }
        });
    }
}

/// What the Editor access section shows, and the one thing it can ask for.
///
/// A plain value rather than the resources themselves, because the section is
/// drawn inside a closure that already holds the panel's `Ui`.
///
/// The **type** exists on every target so the one call site needs no `cfg`; its
/// **fields** do not, because a target with no listener has nothing to put in
/// them and this crate denies dead code. Off the desktop it is an empty struct
/// handed to a section that draws nothing.
#[derive(Default)]
struct EditorRow {
    /// `None` on a build with no listener at all — a phone, a browser, a
    /// scripted capture — where the whole section is skipped.
    #[cfg(mcp)]
    serving: Option<EditorServing>,
    /// Whose chat an agent may read, as typed (`mcp::agent::Audience`), and
    /// whether `--mcp-allow` fixed it for this run.
    #[cfg(mcp)]
    allow: String,
    #[cfg(mcp)]
    allow_pinned: bool,
    /// The list after an edit this frame; written back after the panel.
    #[cfg(mcp)]
    allow_edited: Option<String>,
}

/// The mirror of `mcp::Serving`, so this file does not need that module's types
/// in its signatures.
#[cfg(mcp)]
enum EditorServing {
    At(std::path::PathBuf),
    Off,
    Failed(String),
}

/// Where an editor reaches this client (`systems::mcp`): the socket, and the
/// line to paste into the editor's MCP configuration.
///
/// There is nothing secret to show. The socket sits in a directory only this
/// user can open, which is all the bearer token it replaced ever protected
/// against — so the row is a path and a command, both safe to copy anywhere.
///
/// Nothing to draw on a target that has no listener — see the desktop arm below
/// for what this would be.
#[cfg(not(mcp))]
fn editor_access_section(_ui: &mut egui::Ui, _prefs: &mut AppPrefs, _editor: &mut EditorRow) {}

/// **Drawn wherever there could be a listener**, which is every desktop run —
/// the switch is the point, and a switch you cannot find is not one. It is
/// skipped entirely on a phone, in a browser and under a scripted capture, where
/// there is no listener to explain and a row about one would be a setting that
/// does nothing.
///
/// The socket and the editor line are shown only while it is actually serving:
/// they are what you copy into an editor, and copying them off a client that is
/// switched off is a configuration that silently will not work.
#[cfg(mcp)]
fn editor_access_section(ui: &mut egui::Ui, prefs: &mut AppPrefs, editor: &mut EditorRow) {
    let Some(serving) = &editor.serving else {
        return;
    };
    ui.add_space(8.0);
    ui.separator();
    section(ui, "Editor access");
    ui.add(
        egui::Label::new(
            egui::RichText::new(
                "An editor on this machine can drive this client — fly the camera, \
                 photograph what it sees, read what the map is doing. It listens on a \
                 socket only your own user account can open.",
            )
            .size(theme::SIZE_SUB)
            .color(theme::TEXT_DIM),
        )
        .wrap(),
    );
    theme::checkbox(
        ui,
        &mut prefs.mcp_enabled,
        "Let an editor drive this client",
    );

    let listen = match serving {
        EditorServing::At(path) => path,
        EditorServing::Off => {
            theme::note(
                ui,
                "Switched off. Nothing is listening, and an editor cannot attach.",
            );
            return;
        }
        // Almost always a second client already serving there. Said plainly,
        // because the reader's next move — close the other one, or switch this
        // off and on again — depends on knowing which it was.
        EditorServing::Failed(why) => {
            ui.colored_label(theme::WARN, format!("Not listening: {why}"));
            theme::note(
                ui,
                "Another client on this machine is probably serving there. Switch this \
                 off and on again to try once more.",
            );
            return;
        }
    };

    ui.add_space(4.0);
    for (label, value) in [
        ("Socket", listen.display().to_string()),
        // The `mcpServers` entry, by the binary that is running: what works for
        // somebody who unpacked a download, which a bare `cartopolis` on `PATH`
        // would not.
        ("Editor", crate::systems::mcp::editor_config_here(listen)),
    ] {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(label)
                    .size(theme::SIZE_CHIP)
                    .color(theme::TEXT_DIM),
            );
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&value)
                        .size(theme::SIZE_MONO)
                        .monospace()
                        .color(theme::CREAM),
                )
                .truncate(),
            );
            if ui
                .add(egui::Button::new(format!("{}  Copy", icons::COPY)))
                .clicked()
            {
                crate::utils::clipboard::set_text(&value);
            }
        });
    }
    theme::note(
        ui,
        "Paste the editor line into its MCP configuration. From another machine, \
         run `ssh this-machine cartopolis mcp` as the command instead.",
    );

    // Whose words the agent reads. Worded around the risk rather than the
    // mechanism: the person filling this in is deciding who gets to put text in
    // front of their model.
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("People the agent may hear")
            .size(theme::SIZE_CHIP)
            .color(theme::TEXT_DIM),
    );
    if editor.allow_pinned {
        ui.label(
            egui::RichText::new(if editor.allow.is_empty() {
                "nobody"
            } else {
                &editor.allow
            })
            .size(theme::SIZE_MONO)
            .monospace()
            .color(theme::CREAM),
        );
        theme::note(ui, "Set by --mcp-allow for this run.");
    } else {
        let mut text = editor.allow.clone();
        let edit = ui.add(
            egui::TextEdit::singleline(&mut text)
                .id(egui::Id::new("mcp_allow"))
                .hint_text("usernames, separated by commas")
                .desired_width(f32::INFINITY),
        );
        if edit.changed() {
            editor.allow_edited = Some(text);
        }
        theme::note(
            ui,
            "An agent driving this client reads chat only from these accounts; everyone \
             else's lines are withheld from it. Anything it reads can try to instruct it, \
             so leave strangers off. * lets everyone through.",
        );
    }
}

/// The map this device has downloaded, and the button that throws it away.
///
/// **In General rather than in Graphics**, though the symptom that sends people
/// here is usually something they see: a cache is not a rendering setting, and the
/// tab a person opens looking for "where is my downloaded data" is this one.
///
/// Behind [`theme::confirm_button`] like the account deletion, for a much smaller
/// version of the same reason — nothing is destroyed that cannot be downloaded
/// again, but on a metered connection re-downloading a city is a real cost, and
/// this is a phone-sized button in a scrolling list.
fn storage_section(ui: &mut egui::Ui, cache: &CacheReset) -> bool {
    section(ui, "Storage");
    ui.add(
        egui::Label::new(
            egui::RichText::new(
                "The map, the aerial photography, the buildings and the registers are \
                 kept on this device so revisiting a place is quick. Clearing them \
                 fixes a map that has cached something wrong — tiles that flicker, \
                 a layer that will not draw, buildings or ground that look out of \
                 date. The map around you is then rebuilt from a fresh download.",
            )
            .size(theme::SIZE_SUB)
            .color(theme::TEXT_DIM),
        )
        .wrap(),
    );

    // Nothing you would be sorry to lose is in reach of this button, and saying so
    // is most of what makes it safe to press.
    theme::note(
        ui,
        "Your saved places, photos and settings are not touched.",
    );

    ui.add_space(4.0);
    if cache.running() {
        // Disabled rather than hidden: a button that vanishes under the finger
        // reads as a mis-tap. The repaint request is what keeps this row from
        // sitting on "Clearing…" after the worker has finished — the clear runs
        // off the frame loop, so nothing else would redraw the panel.
        ui.ctx().request_repaint();
        ui.add_enabled_ui(false, |ui| theme::action_button(ui, "Clearing…"));
        return false;
    }

    let pressed = theme::confirm_button(
        ui,
        egui::Id::new("reset_cache_confirm"),
        "Clear downloaded map data",
        "Clear it",
    );

    if let Some(freed) = cache.freed_bytes() {
        // Zero is the browser's honest answer as well as "there was nothing there"
        // (`storage::ClearProgress::freed_bytes`), so it is phrased not to claim a
        // measurement it does not have.
        theme::note(
            ui,
            &match freed {
                0 => "Cleared.".to_string(),
                bytes => format!("Cleared, freeing {}.", human_bytes(bytes)),
            },
        );
    }
    pressed
}

/// Bytes as a person reads them. One decimal place past a megabyte, none below —
/// "412 KB" and "1.4 GB" both being the answer to "is that a lot?".
fn human_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} bytes")
    }
}

/// World: the clock and the weather over the map.
///
/// **Both were the developer panel's**, behind Ctrl+Shift+D — on a client whose
/// main platform has no keyboard, and for two controls that are not developer
/// questions at all. "What time is it here" and "make it rain" are the first two
/// things anybody looking at a city reaches for; the panel keeps what actually
/// needs a developer, which is measurement (see
/// [`crate::systems::dev_panel::render_dev_panel`]).
///
/// **Laid out tight on purpose.** The old block spent eleven rows, four
/// paragraphs and two separators on this; here each group is a mode row, its
/// controls, and one dim line. Three things do the compacting and each is worth
/// keeping: the readout is *inside* the slider (`custom_formatter`) instead of on
/// a label under it, so the clock does not cost a row that changes height as the
/// number changes; the four time presets are one wrapped row of small buttons;
/// and the row spacing is tightened for the tab rather than for the app, because
/// a stack of sliders is the one layout egui's default 6 pt gap makes airy.
/// A phone's 609 pt of viewport is the budget, and this fits it without a scroll.
///
/// The manual controls are **disabled, not hidden**, outside their manual mode.
/// They read the automatic value, so the tab always says what the scene is doing;
/// a control that vanishes reads as a bug, which is the same call
/// [`sky_section`] and [`tri_state`] make.
fn world_tab(
    ui: &mut egui::Ui,
    dev: &mut crate::resources::DevPanelState,
    weather_live: &mut bool,
    observed: Option<crate::systems::weather::Observation>,
) {
    ui.spacing_mut().item_spacing.y = 4.0;

    section(ui, "Time");
    // `Manual` belongs here now. It used to be left out of this tab because
    // its controls were in the other panel, which made it a mode you could
    // land in and not get out of — the old copy literally told the reader to
    // go and find the developer panel.
    crate::systems::theme::segmented(
        ui,
        &mut dev.sun_mode,
        &[
            (SunMode::Auto, "Automatic"),
            (SunMode::Day, "Day"),
            (SunMode::Night, "Night"),
            (SunMode::Manual, "Manual"),
        ],
    );

    // The four presets sit **outside** the manual gate, because clicking one is
    // the natural way to *enter* Manual — they are a mode row as much as the row
    // above is. Disabling them under Automatic would mean picking "Manual" to
    // reach a "Sunset" button whose whole job is to mean "manual, at sunset".
    ui.horizontal_wrapped(|ui| {
        for (label, hour) in [
            ("Sunrise", 6.0),
            ("Noon", 12.0),
            ("Sunset", 18.0),
            ("Midnight", 0.0),
        ] {
            if theme::action_button(ui, label).clicked() {
                dev.time_of_day = hour;
                // Without this the value is written and `apply_sun_mode`
                // overwrites it on the next frame, i.e. the button does nothing
                // visible and reads as broken.
                dev.sun_mode = SunMode::Manual;
            }
        }
    });

    let manual = dev.sun_mode == SunMode::Manual;
    ui.add_enabled_ui(manual, |ui| {
        ui.add(
            egui::Slider::new(&mut dev.time_of_day, 0.0..=24.0)
                .step_by(0.05) // three minutes
                .custom_formatter(|h, _| clock_hhmm(h as f32))
                .custom_parser(parse_clock),
        );
        ui.add(
            egui::Slider::new(
                &mut dev.moon_age_days,
                0.0..=crate::utils::sun::LUNAR_CYCLE_DAYS,
            )
            .text("moon")
            // Both numbers, and both derived from the formatter's own argument
            // rather than from the resource — during a drag the resource still
            // holds last frame's value, so a phase read off it would lag the
            // handle being dragged.
            .custom_formatter(|days, _| {
                let lit = crate::utils::sun::moon_illuminated_fraction(days as f32);
                format!("{days:.1} d · {:.0}% lit", lit * 100.0)
            }),
        );
        ui.horizontal(|ui| {
            theme::checkbox(ui, &mut dev.auto_cycle, "Run the day");
            ui.add(
                egui::DragValue::new(&mut dev.cycle_seconds)
                    .suffix(" s")
                    .range(1.0..=120.0)
                    .speed(0.5),
            );
        });
    });
    pending(
        ui,
        match dev.sun_mode {
            SunMode::Auto => "Sun and moon follow the real time where the map is anchored.",
            SunMode::Day => "Held at noon.",
            SunMode::Night => "Held at midnight.",
            SunMode::Manual => "The clock is yours. Run the day loops it in the seconds beside it.",
        },
    );

    ui.add_space(6.0);
    ui.separator();
    section(ui, "Weather");
    crate::systems::theme::segmented(ui, weather_live, &[(true, "Automatic"), (false, "Manual")]);

    // Deliberately *outside* the Manual-only block below. The sliders are the
    // manual sky and are rightly dead in Automatic mode, but "don't draw clouds"
    // is a standing choice about the picture, and in Automatic mode it is the only
    // one available — the met.no poll owns `cloud_cover` there and would overwrite
    // a zeroed slider on its next fetch. It is also the only control that reaches
    // the space-tier shell at all (see `DevPanelState::clouds_enabled`).
    let clouds_forced_off = crate::env_flag("CARTO_NO_CLOUDS");
    ui.add_enabled_ui(!clouds_forced_off, |ui| {
        ui.checkbox(&mut dev.clouds_enabled, "Clouds");
    });
    pending(
        ui,
        if clouds_forced_off {
            "Forced off for this run by CARTO_NO_CLOUDS."
        } else {
            "Draws the cloud deck over the city and the cloud layer over the \
             globe. Turning it off is the cheapest thing on this tab — from \
             orbit the cloud shell covers the whole screen."
        },
    );

    ui.add_enabled_ui(!*weather_live, |ui| {
        ui.add(
            egui::Slider::new(&mut dev.cloud_cover, 0.0..=1.0)
                .text("cloud")
                .custom_formatter(percent)
                .custom_parser(parse_percent),
        );
        ui.add(
            egui::Slider::new(&mut dev.precipitation, 0.0..=1.0)
                .text("rain")
                .custom_formatter(percent)
                .custom_parser(parse_percent),
        );
        ui.add(
            egui::Slider::new(&mut dev.fog_density, 0.0..=1.0)
                .text("fog")
                .custom_formatter(percent)
                .custom_parser(parse_percent),
        );
    });
    pending(
        ui,
        &match (*weather_live, observed) {
            // The sample point, not just the sample. It is the map *anchor*'s
            // weather, which is not where the camera is looking and can be a long
            // way from it — so a reading that disagrees with the window beside you
            // has an explanation here rather than looking like a bad forecast.
            // (It is also the only reader those two fields have; the developer
            // panel's hover text was it before this tab existed.)
            (true, Some(o)) => format!(
                "Live from api.met.no at {:.3}, {:.3}: {:.0}% cloud · {:.1} mm/h · \
                 {:.0} °C · {:.0} m/s wind.",
                o.lat,
                o.lng,
                o.cloud_cover * 100.0,
                o.precipitation * crate::systems::weather::RAIN_FULL_MM,
                o.air_temp_c,
                o.wind_ms,
            ),
            (true, None) => "Fetching the conditions at the map anchor…".to_string(),
            (false, _) => "The sliders are the sky. Wind rides cloud cover, so the sea \
                           state moves with it."
                .to_string(),
        },
    );
}

/// `13.5` → `"13:30"`. A slider formatter rather than a label under the slider:
/// the value belongs on the control it belongs to, and a separate row would
/// change height with the number.
fn clock_hhmm(hours: f32) -> String {
    let h = hours.floor() as i32 % 24;
    let m = ((hours.fract() * 60.0).floor() as i32).clamp(0, 59);
    format!("{h:02}:{m:02}")
}

/// `"13:30"` → `13.5`, the inverse of [`clock_hhmm`].
///
/// **A custom formatter without a matching parser is a trap**, not a cosmetic
/// gap: egui lets you double-click a slider and type into it, and the text it
/// puts there is whatever the formatter produced. Left to the default parser,
/// typing over `"13:30"` reads `13` and silently discards the minutes — so the
/// control disagrees with itself. `None` on anything unparseable leaves the value
/// alone, which is egui's own convention.
fn parse_clock(text: &str) -> Option<f64> {
    let text = text.trim();
    let (h, m) = match text.split_once(':') {
        Some((h, m)) => (h.trim().parse::<f64>().ok()?, m.trim().parse::<f64>().ok()?),
        // A bare number is hours, so "18" still works.
        None => (text.parse::<f64>().ok()?, 0.0),
    };
    Some(h + m / 60.0)
}

/// A 0..1 slider read as a percentage. Shared by the three weather sliders so
/// they cannot drift into three roundings of the same number.
fn percent(value: f64, _: std::ops::RangeInclusive<usize>) -> String {
    format!("{:.0}%", value * 100.0)
}

/// `"60%"` → `0.6`, the inverse of [`percent`]. See [`parse_clock`] for why the
/// inverse has to exist: without it, typing over `"60%"` sets the value to 60,
/// which the range then clamps to 1.0 — i.e. every typed number means "full".
fn parse_percent(text: &str) -> Option<f64> {
    text.trim()
        .trim_end_matches('%')
        .trim()
        .parse::<f64>()
        .ok()
        .map(|v| v / 100.0)
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
/// detail preset, then the render scale and frame rate, then the individual
/// features. **Everything here applies at once** (2026-09-29): the preset swaps
/// the budget (`quality::select`) and `render_features` reconciles the camera to
/// every row, the sky included — see its module note for how the sky got there.
/// The individual rows are `Option`s whose `None` follows the preset, so picking
/// a preset moves every row still on Automatic and none that was set by hand.
fn graphics_tab(
    ui: &mut egui::Ui,
    prefs: &mut AppPrefs,
    target: Option<UVec2>,
    sky: Option<crate::systems::sky_dome::SkyMode>,
) {
    section(ui, "Detail");
    let presets = QualityPreset::ALL.map(|p| (p, p.label()));
    crate::systems::theme::segmented(ui, &mut prefs.quality, &presets);
    pending(
        ui,
        "Applies immediately. Automatic picks by device. The settings below that \
         are on Automatic follow it.",
    );

    ui.add_space(8.0);
    ui.separator();
    render_scale_row(ui, prefs, target);

    ui.add_space(8.0);
    frame_rate_row(ui, prefs);

    ui.add_space(8.0);
    ui.separator();
    section(ui, "Effects");
    tri_state(
        ui,
        &mut prefs.bloom,
        crate::quality::budget().bloom,
        "Bloom",
    );
    // A run launched with neither bloom nor the full sky has no `Hdr`, and that
    // cannot be added to a live camera (`render_features::hdr_at_launch`).
    let hdr = crate::systems::render_features::hdr_at_launch().unwrap_or(true);
    ui.label(
        egui::RichText::new(if !hdr && prefs.bloom_on() {
            "Turns on at the next start: this run was launched with the simple \
             sky and no bloom."
        } else {
            "Glare around the sun, lit windows and the planet's edge from orbit. \
             Thirteen full-screen passes when on."
        })
        .small()
        .color(theme::TEXT_DIM),
    );
    ui.add_space(6.0);
    tri_state(
        ui,
        &mut prefs.anti_aliasing,
        crate::quality::budget().msaa != bevy::render::view::Msaa::Off,
        "Anti-aliasing",
    );
    ui.label(
        egui::RichText::new(
            "Smooth edges. Applies immediately as an edge filter; multisampling, \
             which is sharper and costs more memory traffic, follows at the next start.",
        )
        .small()
        .color(theme::TEXT_DIM),
    );

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
            "The sun's shadow maps. Applies immediately — the light is respawned."
        })
        .small()
        .color(theme::TEXT_DIM),
    );

    // Read before the rows below, because `tri_state` borrows `prefs` mutably and
    // the label beside each row has to say what "Automatic" resolved to.
    let caps = crate::quality::capabilities();
    let shafts_possible_adapter = caps.is_some_and(|caps| !caps.software_renderer);
    let shadows_now = prefs.shadows_on();
    let prefs_shafts_default = shafts_possible_adapter && shadows_now;

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
             reflect. Costs a cubemap re-baked every frame. Applies immediately."
        } else {
            "Needs the full atmosphere — the simple sky has nothing to reflect."
        })
        .small()
        .color(theme::TEXT_DIM),
    );

    ui.add_space(6.0);
    // This one falls back to the **adapter**, not to the tier, so the "Automatic"
    // value shown beside the checkbox is what this machine can do rather than what
    // its class is assumed to want.
    tri_state(
        ui,
        &mut prefs.light_shafts,
        prefs_shafts_default,
        "Light shafts",
    );
    ui.label(
        egui::RichText::new(if !shafts_possible_adapter {
            "Needs a hardware graphics adapter — this one rasterises on the CPU, \
             where the ray march does not finish."
        } else if !shadows_now {
            "Needs shadows: without a shadow map there is nothing to cut a shaft \
             out of, and the effect is a flat wash."
        } else {
            "Sunlight picked out by the air, in the volume around you. The most \
             expensive setting here — a ray march that samples the sun's shadows \
             at every step. Applies immediately."
        })
        .small()
        .color(theme::TEXT_DIM),
    );

    ui.add_space(6.0);
    // Adapter-resolved like the shafts, but not gated on shadows: the occluders
    // are whatever is darker than the sky in the finished frame.
    tri_state(ui, &mut prefs.sun_rays, shafts_possible_adapter, "Sun rays");
    ui.label(
        egui::RichText::new(if shafts_possible_adapter {
            "Sunbeams fanning out from the sun through gaps in the clouds and past \
             buildings. One full-screen pass over the finished frame. Applies \
             immediately."
        } else {
            "Needs a hardware graphics adapter — this one rasterises on the CPU."
        })
        .small()
        .color(theme::TEXT_DIM),
    );

    ui.add_space(8.0);
    ui.separator();
    // **Neither of the two sections that used to close this tab is here any
    // more, and they left for opposite reasons.** The performance overlay — a
    // box of frame rate and entity count anchored over the map — is gone
    // entirely: it existed because the panel that already had those numbers was
    // behind a keyboard chord, and that stopped being true when Developer tools
    // got a main-menu entry (`systems::dev::dev_panel`). The `CARTO_*` bisecting
    // switches moved into that panel too, because they were never settings: a
    // run with one on is a run whose rendering proves nothing, which is not a
    // thing to keep beside a shadow toggle.
    theme::note(
        ui,
        "Frame rate, draw counts, GPU uploads and the startup switches for \
         bisecting a render problem are in Developer tools — in the menu, or \
         Ctrl+Shift+D.",
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
    let skies = SkyPreference::ALL.map(|s| (s, s.label()));
    crate::systems::theme::segmented(ui, &mut prefs.sky, &skies);
    pending(
        ui,
        "Applies immediately. Simple is cheaper and works on every device.",
    );

    // What this run is drawing, and — when it differs from the request — why. The
    // capability is the only reason the answer can be narrower than the ask.
    let filterable = crate::quality::capabilities().map(|c| c.float32_filterable);
    let now = match sky {
        Some(SkyMode::Atmosphere) => "Drawing the full atmosphere.".to_string(),
        Some(SkyMode::Dome) => {
            let launched_without_hdr =
                crate::systems::render_features::hdr_at_launch() == Some(false);
            let why = match (prefs.sky, filterable) {
                (SkyPreference::Simple, _) => "as asked",
                (_, Some(false)) => "this GPU cannot sample the atmosphere's lookup textures",
                _ if launched_without_hdr && SkyMode::atmosphere_possible() => {
                    "the full sky turns on at the next start, since this run was \
                     launched with the simple sky and no bloom"
                }
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

/// Frames per second to aim for: Automatic, a fixed rate, or no cap.
///
/// Buttons rather than a slider, because on a phone the number is a request to
/// the display, which rounds it to a mode it has — 45 on a 60/120 Hz panel is not
/// a thing anyone gets. The idle halving in `frame_pacing` still applies on top.
fn frame_rate_row(ui: &mut egui::Ui, prefs: &mut AppPrefs) {
    const CHOICES: [(Option<u32>, &str); 5] = [
        (None, "Automatic"),
        (Some(30), "30"),
        (Some(60), "60"),
        (Some(120), "120"),
        (Some(0), "No cap"),
    ];
    ui.label("Frame rate");
    crate::systems::theme::segmented(ui, &mut prefs.frame_rate, &CHOICES);
    let note = match prefs.frame_rate {
        None => {
            let fps = crate::quality::budget().target_fps();
            if fps > 0.0 {
                format!("Automatic — {fps:.0} fps for this device. Lower saves battery and heat.")
            } else {
                "Automatic — no cap for this device.".to_string()
            }
        }
        Some(0) => "As fast as the device can draw. Uses the most battery.".to_string(),
        Some(_) => "Lower saves battery and heat.".to_string(),
    };
    ui.label(egui::RichText::new(note).small().color(theme::TEXT_DIM));
}

/// The render-scale slider. Applies immediately: `RenderScale` is a `Resource`
/// the render world re-extracts every frame.
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
    microphone_section(ui, state, prefs);
    camera_section(ui, state);
    play_tone
}

fn microphone_section(ui: &mut egui::Ui, state: &mut SettingsState, prefs: &mut AppPrefs) {
    section(ui, "Microphone");

    if !AudioInput::available() {
        // The `platform::location` contract: say unavailable rather than show a
        // meter that can never move.
        ui.colored_label(theme::ERROR, unavailable_reason());
        return;
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
                format!("{}  Stop microphone", icons::STOP)
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

    if state.mic.is_none() {
        if let Some((meter, peak)) = state.demo_level {
            level_bar(ui, meter, peak);
        }
    }
    let (now, dt) = ui.input(|i| (i.time, i.stable_dt));
    if let Some((meter, peak)) = observe_peak(state, prefs.capture_gain(), now, dt) {
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
            // The bar falls back smoothly, so it has to be redrawn while it does.
            ui.ctx().request_repaint();
        }
    }
}

/// The camera test: the picture a call would send, shown mirrored as a mirror
/// shows you (the other person sees you the right way round).
fn camera_section(ui: &mut egui::Ui, state: &mut SettingsState) {
    ui.add_space(10.0);
    ui.separator();
    section(ui, "Camera");
    #[cfg(target_arch = "wasm32")]
    {
        let _ = state;
        ui.label(
            "In the browser, the camera is the browser's: it asks to use it when you \
             turn the camera on in a call.",
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use crate::platform::camera;
        let running = state.camera_running();
        if ui
            .button(if running {
                format!("{}  Stop camera", icons::STOP)
            } else {
                format!("{}  Test camera", icons::CAMERA)
            })
            .clicked()
        {
            if running {
                state.stop_camera();
            } else {
                state.start_camera();
            }
        }
        let Some(cam) = state.camera.as_ref() else {
            return;
        };
        if let Some(err) = cam.error() {
            ui.colored_label(theme::ERROR, err);
            return;
        }
        if let Some(picture) = cam.take_preview() {
            let (w, h, rgba) = camera::self_view(&picture, 480);
            let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
            match state.camera_texture.as_mut() {
                Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                None => {
                    state.camera_texture = Some(ui.ctx().load_texture(
                        "camera-test",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
        }
        match state.camera_texture.as_ref() {
            Some(t) => {
                let [w, h] = t.size();
                // Enough to judge framing and light, without pushing the rest
                // of the tab off the screen.
                let aspect = w as f32 / h.max(1) as f32;
                let height = (ui.available_width() / aspect).min(200.0);
                let size = egui::vec2(height * aspect, height);
                ui.add(
                    egui::Image::new((t.id(), size))
                        .corner_radius(egui::CornerRadius::same(theme::RADIUS_SM)),
                );
                ui.small("Mirrored, as a mirror shows you. The other person sees you the right way round.");
            }
            None => {
                ui.label("Waiting for the camera…");
            }
        }
        ui.ctx().request_repaint();
    }
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

/// Segments in [`level_bar`].
const METER_SEGMENTS: usize = 30;

/// A level meter: a row of lit segments, like a mixing desk's, with a
/// peak-hold segment above the signal.
///
/// Every segment is drawn, lit or not: the unlit ones in a faint copy of their
/// own colour, so the scale — calm, then caution, then clipping — reads before
/// any sound arrives. A lit segment carries a soft glow. Its colour is the
/// colour of where it *is* on the scale ([`meter_colour`]), not of the current
/// level, which is what makes a loud passage climb into amber and red rather
/// than turning the whole bar red.
///
/// Shared with the chat panel's voice row rather than redrawn there: the two
/// answer the same question about the same device, and a second bar with its own
/// colours is how the chrome drifts (`systems::theme`'s note).
pub(crate) fn level_bar(ui: &mut egui::Ui, meter: f32, peak: f32) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, egui::CornerRadius::same(theme::RADIUS_SM), theme::INK);
    let inner = rect.shrink2(egui::vec2(4.0, 4.0));
    let gap = 2.0;
    let n = METER_SEGMENTS as f32;
    let width = ((inner.width() - gap * (n - 1.0)) / n).max(1.0);
    let lit_upto = (meter.clamp(0.0, 1.0) * n).round() as usize;
    let peak_at = (peak.clamp(0.0, 1.0) * n).ceil() as usize;
    for i in 0..METER_SEGMENTS {
        let x = inner.min.x + i as f32 * (width + gap);
        let segment = egui::Rect::from_min_size(
            egui::pos2(x, inner.min.y),
            egui::vec2(width, inner.height()),
        );
        let colour = segment_colour((i as f32 + 0.5) / n);
        let radius = egui::CornerRadius::same(2);
        let lit = i < lit_upto;
        let is_peak = peak > 0.0 && i + 1 == peak_at && !lit;
        if lit {
            p.rect_filled(
                segment.expand(1.5),
                egui::CornerRadius::same(3),
                colour.gamma_multiply(0.25),
            );
            p.rect_filled(segment, radius, colour);
        } else if is_peak {
            p.rect_filled(segment, radius, colour.gamma_multiply(0.85));
        } else {
            p.rect_filled(segment, radius, colour.gamma_multiply(0.12));
        }
    }
}

/// A segment's colour by where it sits on the scale: the calm range
/// brightening as it climbs, then [`meter_colour`]'s caution and clipping.
fn segment_colour(at: f32) -> egui::Color32 {
    if at > 0.75 {
        return meter_colour(at);
    }
    let t = (at / 0.75).clamp(0.0, 1.0);
    let low = egui::Rgba::from(theme::ACCENT_PRESSED);
    let high = egui::Rgba::from(theme::ACCENT_BRIGHT);
    egui::Color32::from(low * (1.0 - t) + high * t)
}

/// The live tone through amber to the error red as the signal approaches
/// clipping.
///
/// The top of the bar is a warning, not a target — a meter that is full-scale in
/// the ordinary colour invites people to clip their own microphone. This is one
/// of the places the retired second accent was doing `WARN`'s job: a meter three
/// quarters of the way up is a caution, and caution is one of the two things the
/// monochrome palette still says in colour.
fn meter_colour(meter: f32) -> egui::Color32 {
    if meter > 0.9 {
        theme::ERROR
    } else if meter > 0.75 {
        theme::WARN
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
/// In the panel `.chain()` with the other panels — it draws into the shared
/// root `Ui`, so leaving it out is the dead-click failure that note describes.
pub fn render_about_panel(
    mut egui: EguiMut,
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

    /// The World tab's clock formatter and its parser must be inverses, because
    /// egui puts the formatter's output into the text box the parser reads back
    /// when a slider is double-clicked. A pair that disagrees is a control that
    /// changes its own value when you look at it.
    #[test]
    fn the_clock_round_trips_through_its_own_text() {
        for hours in [0.0_f32, 6.0, 12.5, 13.5, 18.25, 23.75] {
            let text = clock_hhmm(hours);
            let back = parse_clock(&text).expect("the formatter's own output must parse");
            assert!(
                (back - hours as f64).abs() < 1.0 / 60.0,
                "{hours} → {text} → {back}"
            );
        }
        // A bare hour is still accepted, so typing "18" does the obvious thing.
        assert_eq!(parse_clock("18"), Some(18.0));
        assert_eq!(parse_clock(" 07:30 "), Some(7.5));
        // Unparseable leaves the value alone rather than zeroing it.
        assert_eq!(parse_clock("noon"), None);
    }

    /// Same contract for the weather sliders. The failure this pins is specific:
    /// without a parser, typing over "60%" yields 60, which the 0..=1 range
    /// clamps to 1.0 — so every typed number would mean "full".
    #[test]
    fn a_percentage_round_trips_and_does_not_clamp_to_full() {
        for value in [0.0_f64, 0.15, 0.6, 1.0] {
            let text = percent(value, 0..=0);
            let back = parse_percent(&text).expect("the formatter's own output must parse");
            assert!((back - value).abs() < 0.005, "{value} → {text} → {back}");
        }
        assert_eq!(
            parse_percent("60"),
            Some(0.6),
            "a bare number is a percentage"
        );
        assert_eq!(parse_percent(""), None);
    }

    #[test]
    fn peak_holds_the_maximum_and_resets_with_the_test() {
        let mut state = SettingsState::default();
        // No mic: nothing to observe, and no panic.
        assert!(observe_peak(&mut state, 1.0, 0.0, 0.016).is_none());

        state.peak = 0.6;
        state.stop_mic();
        assert_eq!(state.peak, 0.0, "stopping a test clears the peak hold");
    }

    #[test]
    fn meter_warns_before_it_reaches_the_end() {
        // The bar must change colour below full scale, or it reads as a target.
        assert_eq!(meter_colour(0.5), theme::ACCENT);
        assert_eq!(meter_colour(0.8), theme::WARN);
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
            SettingsTab::World,
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

    /// The receiver readout names the problem a person can act on, rather than
    /// "connected": the port's own error, a wrong baud rate, and a caster that
    /// failed — and says RTK fixed with centimetres when all is well.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_receiver_readout_says_what_is_wrong_and_what_is_right() {
        use cartopolis_gnss::{CasterState, FixQuality, Link, Solution, Status};
        use std::time::Instant;

        let failed = Status {
            link: Link::Failed("no permission to open /dev/ttyUSB0".into()),
            ..Status::default()
        };
        let lines = crate::systems::gnss_receiver::status_lines(&failed);
        assert_eq!(
            lines,
            vec![(true, "No permission to open /dev/ttyUSB0".to_string())]
        );

        let mut garbled = Status {
            link: Link::Open("FT230X (/dev/ttyUSB0), 9600 baud".into()),
            bytes_in: 10_000,
            ..Status::default()
        };
        assert!(crate::systems::gnss_receiver::status_lines(&garbled)
            .iter()
            .any(|(warn, l)| *warn && l.contains("baud rate")));

        garbled.sentences = 50;
        garbled.solution_at = Some(Instant::now());
        garbled.solution = Some(Solution {
            lat: 53.2194,
            lng: 6.5665,
            quality: FixQuality::RtkFixed,
            altitude_m: Some(8.1),
            geoid_separation_m: Some(40.5),
            satellites: Some(31),
            hdop: Some(0.6),
            correction_age_s: Some(1.0),
            utc_s: None,
            sigma_m: Some((0.01, 0.01, None)),
            speed_ms: None,
            course_deg: None,
        });
        garbled.corrections.state = CasterState::Streaming;
        garbled.corrections.mountpoint = Some("GRON".into());
        garbled.corrections.messages = vec![(1005, 3), (1077, 30)];
        let lines = crate::systems::gnss_receiver::status_lines(&garbled);
        assert!(lines.iter().all(|(warn, _)| !warn), "{lines:?}");
        assert_eq!(lines[1].1, "RTK fixed · ±1 cm · 31 satellites · HDOP 0.6");
        assert!(
            lines.iter().any(|(_, l)| l == "RTCM 1005 1077"),
            "{lines:?}"
        );
    }
}
