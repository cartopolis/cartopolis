//! The Settings panel's state — and the About panel, which is next to it
//! because they are the two screens that are *about the application* rather
//! than about the world.
//!
//! # Settings
//!
//! The panel itself is the component library's (`lib_settings`), which mounts
//! it, syncs it with the resources it edits and applies its taps. What lives
//! here is what that panel and the rest of the client share: the list of
//! pages ([`SettingsTab`]), the panel's state ([`SettingsState`]), the
//! microphone test's meter ballistics, and [`follow_preset`].
//!
//! Graphics is **what this machine spends on the picture**; World is **what the
//! picture is of**. A new control belongs on whichever side of that answers it,
//! and a control that belongs on neither is probably a measurement, which is
//! what Developer tools kept.
//!
//! **Nothing here invents a backend.** Each control is wired to state that
//! already exists, or to a [`crate::resources::AppPrefs`] field whose doc
//! comment says plainly that nothing reads it yet. The alternative — a switch
//! that appears to work — is worse than an honest one that says "next launch",
//! because the first is a bug report and the second is a fact.
//!
//! # What the audio tests actually establish
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
//! The mic is opened only while the page is showing it and released as soon as
//! the test stops or the panel closes. Holding a microphone open for the lifetime
//! of the process is how an application ends up with a permanent recording
//! indicator in the OS status bar, which users are right to find alarming.
//!
//! # About
//!
//! The version, the build, and the third-party notices — **a licence obligation,
//! not a courtesy**. ODbL (OpenStreetMap and everything derived from it), CC BY
//! 4.0 (3DBAG, MET Norway) and the SIL Open Font License all require the notice
//! to travel with the distributed work.
//!
//! The credits are not typed here. `LICENSE-THIRD-PARTY.md` at the repo root says
//! of itself that it is the canonical list and that "the in-app About panel
//! renders it", so this panel `include_str!`s that file and parses its records
//! ([`parse_notices`]). Adding a data source therefore means editing one file,
//! and a source that is credited in the tarball but not in the app becomes
//! impossible rather than merely unlikely.

use bevy::prelude::*;

use crate::platform::audio::{self, AudioInput};
use crate::systems::icons;

/// Which page of the Settings panel is showing. Settings is a list of these
/// (`docs/mockups/home-and-profile/v1`, D): five tabs in a row was more than a
/// phone's width holds, and the list has room for the two pages that joined —
/// Privacy and safety, and Access tokens.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsTab {
    #[default]
    General,
    /// The world's clock and its weather — what the scene *is*. Called World and
    /// not Map because the map's layers have a panel of their own; this page is
    /// the time of day, the season and the sky over it.
    World,
    /// What this machine spends on the picture. Everything here used to be in the
    /// developer panel, where it was both unreachable to the people who needed it
    /// most and unpersisted.
    Graphics,
    Audio,
    Account,
    /// Who may message you, who you blocked, and how to report somebody. The
    /// block list was at the foot of the chat list until 2026-10-16.
    Privacy,
    /// The access tokens a bot or a script signs in with (`access_tokens`).
    Tokens,
}

impl SettingsTab {
    /// Every page, in the list's order: the account's, then the app's. The
    /// list is drawn from this, so a page left out of it cannot be reached.
    #[cfg(feature = "comms")]
    pub(crate) const ALL: [Self; 7] = [
        Self::Privacy,
        Self::Tokens,
        Self::Account,
        Self::General,
        Self::Audio,
        Self::Graphics,
        Self::World,
    ];
    /// The web build's: the account's pages are the page's around it, which
    /// signed it in (`comms`).
    #[cfg(not(feature = "comms"))]
    pub(crate) const ALL: [Self; 4] = [Self::General, Self::Audio, Self::Graphics, Self::World];

    /// Under "Account" in the list, rather than "App".
    pub(crate) fn about_the_account(self) -> bool {
        matches!(self, Self::Privacy | Self::Tokens | Self::Account)
    }

    pub(crate) fn icon(self) -> &'static str {
        match self {
            Self::General => icons::SETTINGS,
            Self::World => icons::CAT_ORBIT,
            Self::Graphics => icons::PALETTE,
            Self::Audio => icons::MIC,
            Self::Account => icons::APPEARANCE,
            Self::Privacy => icons::LOCK,
            Self::Tokens => icons::KEY,
        }
    }

    /// The line under the page's name in the list.
    pub(crate) fn hint(self) -> Option<&'static str> {
        match self {
            Self::General => Some("Units, navigation, connection"),
            Self::World => None,
            Self::Graphics => None,
            Self::Audio => Some("Volume, microphone, camera"),
            Self::Account => None,
            Self::Privacy => Some("Blocked people, who can message me"),
            Self::Tokens => Some("For bots and scripts"),
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::World => "Time and weather",
            Self::Graphics => "Graphics",
            Self::Audio => "Sound and voice",
            Self::Account => "Account",
            Self::Privacy => "Privacy and safety",
            Self::Tokens => "Access tokens",
        }
    }
}

/// Panel state, plus the live microphone while a mic test is running.
#[derive(Resource, Default)]
pub struct SettingsState {
    /// The page showing; `None` is the list of them.
    pub page: Option<SettingsTab>,
    /// `Some` only while the mic test is active — see the module note on why
    /// this is not held open for the process lifetime.
    pub mic: Option<AudioInput>,
    /// The meter as drawn: rises with the signal at once and falls back
    /// smoothly, as a hardware meter's needle does.
    pub shown: f32,
    /// The peak-hold mark: the highest recent level, held for a second, then
    /// falling.
    pub peak: f32,
    /// When the peak mark was last raised (seconds of app time).
    pub peak_at: f64,
    /// A level to draw on the meter with no microphone running: a scripted
    /// capture's (`--ui audio`), which has no device to move it. (meter, peak)
    pub demo_level: Option<(f32, f32)>,
    /// The password re-entered to confirm account deletion.
    ///
    /// Cleared the moment the request goes out and whenever the panel closes: it
    /// is the one string in this struct that must not outlive the interaction
    /// that produced it.
    pub delete_password: String,
    /// The camera, while the camera test runs.
    #[cfg(not(target_arch = "wasm32"))]
    pub camera: Option<crate::platform::camera::Camera>,
    /// The size of the camera test's last picture: `None` until the first
    /// arrives, which is "Waiting for the camera…" on the page.
    pub camera_size: Option<[u32; 2]>,
    /// The GNSS receiver's settings as being edited, applied with a button —
    /// a receiver restarted on every keystroke of a caster's host name would
    /// spend the typing reconnecting. `None` until the section is first shown.
    pub receiver_draft: Option<crate::systems::gnss_receiver::ReceiverPrefs>,
    /// The serial ports last listed, and when (seconds of app time): listing
    /// reads `/sys`, which is not something to do sixty times a second.
    /// Desktop only — a phone's adapter is not chosen from a list.
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
        self.camera_size = None;
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
        self.camera_size = None;
    }

    /// Whether the camera test is running.
    #[must_use]
    pub fn camera_running(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        return self.camera.is_some();
        #[cfg(target_arch = "wasm32")]
        false
    }

    /// What went wrong opening the camera, while the test runs.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn camera_error(&self) -> Option<String> {
        self.camera.as_ref().and_then(|c| c.error())
    }

    /// The camera test's newest picture, when one arrived since the last
    /// call: mirrored as a mirror shows you (the other person sees you the
    /// right way round), as RGBA at most 480 wide, with its size.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn take_camera_picture(&mut self) -> Option<(Vec<u8>, [u32; 2])> {
        let picture = self.camera.as_ref()?.take_preview()?;
        let (w, h, rgba) = crate::platform::camera::self_view(&picture, 480);
        self.camera_size = Some([w, h]);
        Some((rgba, [w, h]))
    }

    /// The receiver's settings as being edited, begun from the applied ones.
    pub fn receiver_draft(
        &mut self,
        prefs: &crate::systems::gnss_receiver::ReceiverPrefs,
    ) -> &mut crate::systems::gnss_receiver::ReceiverPrefs {
        self.receiver_draft.get_or_insert_with(|| prefs.clone())
    }

    /// **The switch applies at once; everything under it waits for Apply.**
    /// The switch is the one control whose effect a person expects
    /// immediately, and the rest are text that is wrong until it is finished.
    pub fn set_receiver_enabled(
        &mut self,
        prefs: &mut crate::systems::gnss_receiver::ReceiverPrefs,
        on: bool,
    ) {
        prefs.enabled = on;
        self.receiver_draft(prefs).enabled = on;
    }

    /// Apply the draft.
    pub fn apply_receiver(&mut self, prefs: &mut crate::systems::gnss_receiver::ReceiverPrefs) {
        if let Some(draft) = &self.receiver_draft {
            *prefs = draft.clone();
        }
    }

    /// Throw the draft away for the applied settings.
    pub fn revert_receiver(&mut self, prefs: &crate::systems::gnss_receiver::ReceiverPrefs) {
        self.receiver_draft = Some(prefs.clone());
    }

    /// List the serial ports again when the last list is more than three
    /// seconds old. Answers whether it listed.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    pub fn refresh_receiver_ports(&mut self, now: f64) -> bool {
        if self.receiver_ports_at.is_some_and(|t| now - t <= 3.0) {
            return false;
        }
        self.receiver_ports = crate::platform::gnss_receiver::ports();
        self.receiver_ports_at = Some(now);
        true
    }
}

/// The baud rates a receiver can be asked for; none where nothing can be
/// plugged in (a browser tab — `platform::gnss_receiver`).
pub fn receiver_bauds() -> &'static [u32] {
    #[cfg(not(target_arch = "wasm32"))]
    return &crate::systems::gnss_receiver::BAUD_RATES;
    #[cfg(target_arch = "wasm32")]
    &[]
}

/// The output rates a receiver can be asked for, in Hz; none in a browser.
pub fn receiver_rates() -> &'static [u32] {
    #[cfg(not(target_arch = "wasm32"))]
    return &crate::systems::gnss_receiver::RATES_HZ;
    #[cfg(target_arch = "wasm32")]
    &[]
}

/// The line under the baud and rate pickers when the baud rate cannot carry
/// the rate asked for.
pub fn receiver_rate_note(draft: &crate::systems::gnss_receiver::ReceiverPrefs) -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let carried = cartopolis_gnss::receiver::rate_for_baud(draft.rate_hz, draft.baud);
        (carried < draft.rate_hz).then(|| {
            format!(
                "{} baud carries {carried} Hz; the receiver will be asked for that.",
                draft.baud
            )
        })
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = draft;
        None
    }
}

/// What the Storage section says once a reset has finished. Zero is the
/// browser's honest answer as well as "there was nothing there"
/// (`storage::ClearProgress::freed_bytes`), so it is phrased not to claim a
/// measurement it does not have.
pub fn cleared_note(freed: u64) -> String {
    match freed {
        0 => "Cleared.".to_string(),
        bytes => format!("Cleared, freeing {}.", human_bytes(bytes)),
    }
}

/// Bytes as a person reads them. One decimal place past a megabyte, none below —
/// "412 KB" and "1.4 GB" both being the answer to "is that a lot?".
pub fn human_bytes(bytes: u64) -> String {
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

/// Track the peak while a test runs.
///
/// `gain` is [`crate::resources::AppPrefs::capture_gain`], applied here because
/// the meter is the only place the input gain can be judged — voice transmits
/// the gained signal, so a meter showing the raw one would have someone
/// calibrate against a number nobody sends.
pub(crate) fn observe_peak(
    state: &mut SettingsState,
    gain: f32,
    now: f64,
    dt: f32,
) -> Option<(f32, f32)> {
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

/// `13.5` → `"13:30"`: the World page's clock, as its slider shows it.
pub(crate) fn clock_hhmm(hours: f32) -> String {
    let h = hours.floor() as i32 % 24;
    let m = ((hours.fract() * 60.0).floor() as i32).clamp(0, 59);
    format!("{h:02}:{m:02}")
}

/// Every binding the client has, grouped by the mode it works in.
///
/// **There was no control legend anywhere in the application.** WASD, Space,
/// E, Q, Shift, Alt, 1–6, Tab, Delete and three Ctrl chords existed only in
/// `CLAUDE.md` and in commit bodies; the sole in-UI hint was the assistant's
/// "(Ctrl+Enter)" and the `[E]` on the interact prompt. So a desktop user who
/// never guessed a key saw an avatar they could not move, and the keys that are
/// *silently* mode-dependent — Space and the emotes do nothing outside Follow, E
/// meant "interact" in Follow and "up" in Fly — had nothing to explain
/// themselves. (The camera stopped flying on 2026-11-25; Driving took its
/// place.)
///
/// It lives in About because About already exists, is already in the main menu,
/// already scrolls, and ships on the phone — where the gesture half of this
/// table is the part that matters. A new panel for it would be a new thing to
/// discover, which is the problem rather than the fix.
///
/// Grouped **by mode** deliberately: a flat list would say `E` twice and be
/// wrong both times.
pub const CONTROLS: &[(&str, &[(&str, &str)])] = &[
    (
        "Walking (the default camera)",
        &[
            ("W A S D", "Walk — the avatar turns to face where it goes"),
            ("Shift / Alt", "Run / stroll"),
            ("Space", "Jump"),
            (
                "E",
                "Interact with whatever is in front of you — or get into a car",
            ),
            ("E / G", "Carrying something: throw it / put it down"),
            ("C", "Call your car"),
            ("1 – 6", "Emotes: wave, interact, roll, punch, kick, faint"),
        ],
    ),
    (
        "Driving",
        &[
            ("W / S", "Accelerate / brake — held at a stop, S reverses"),
            ("A / D", "Steer"),
            ("Space", "Handbrake"),
            ("E", "Stop and get out"),
        ],
    ),
    (
        "Anywhere",
        &[
            ("Tab", "Next camera: walk → whole Earth → walk"),
            ("Esc", "Close whatever is open, innermost first"),
            ("Ctrl+L", "Search"),
            ("Ctrl+Shift+A", "AI assistant"),
            ("Ctrl+Shift+D", "Developer tools"),
        ],
    ),
    (
        "Touch",
        &[
            ("Drag", "Turn the view"),
            ("Pinch", "Zoom"),
            (
                "Two fingers",
                "Drag to pan, twist to rotate, up/down to tilt",
            ),
            ("Long press", "The same menu a right-click opens"),
        ],
    ),
];

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

/// The safety summary shown in About and on the Privacy and safety page.
///
/// Both places, deliberately. About is where a reviewer looks; Privacy and
/// safety is where a person who has just been harassed looks.
pub const SAFETY_TEXT: &str = "Tap or right-click somebody's name in chat to block or \
     report them. Blocking is immediate and lasts until you undo it; reports go to the \
     people who run this world.";

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

/// The shipped notices, parsed: what the About page lists (`lib_about`).
pub fn notices() -> Vec<Notice> {
    parse_notices(THIRD_PARTY)
}

/// What this binary is, in one line: the platform it was built for, the profile,
/// and the render tier it resolved at startup. All three are the first questions
/// asked of a bug report, and the phone is the one device with no console to ask
/// them from.
pub fn build_line() -> String {
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

/// Swap the budget when Settings ▸ Graphics picks another preset.
///
/// `PreUpdate`, so that every system in `Update` sees the new budget in the same
/// frame it sees the changed preference — `manage_lights` and
/// `apply_preferred_render_scale` both act on `AppPrefs` change detection and
/// would otherwise apply the old budget's answer once and not look again.
pub fn follow_preset(prefs: bevy::prelude::Res<crate::resources::AppPrefs>) {
    if bevy::prelude::DetectChanges::is_changed(&prefs) {
        crate::quality::select(prefs.quality.as_env_word());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The World page's clock reads as hours and minutes, and wraps at
    /// midnight rather than showing a 24th hour.
    #[test]
    fn the_clock_reads_as_hours_and_minutes() {
        assert_eq!(clock_hhmm(0.0), "00:00");
        assert_eq!(clock_hhmm(6.0), "06:00");
        assert_eq!(clock_hhmm(13.5), "13:30");
        assert_eq!(clock_hhmm(18.25), "18:15");
        assert_eq!(clock_hhmm(23.75), "23:45");
        assert_eq!(clock_hhmm(24.0), "00:00");
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

    /// The ballistics: up at once, the peak held for a second, then both fall.
    #[test]
    fn the_meter_rises_at_once_and_its_peak_holds() {
        let mut state = SettingsState::default();
        assert_eq!(meter_ballistics(&mut state, 0.8, 0.0, 0.016), (0.8, 0.8));
        // The signal drops: the bar falls smoothly, the peak holds.
        let (shown, peak) = meter_ballistics(&mut state, 0.0, 0.5, 0.1);
        assert!((shown - (0.8 - METER_FALL * 0.1)).abs() < 1e-6, "{shown}");
        assert_eq!(peak, 0.8, "the peak holds within PEAK_HOLD_S");
        // Past the hold, the peak falls too — never below the bar.
        let (shown, peak) = meter_ballistics(&mut state, 0.0, 2.0, 0.1);
        assert!(peak < 0.8 && peak >= shown, "{peak} {shown}");
    }

    /// Every page must be reachable from the list, and the list is built from
    /// `ALL` — a variant missing from it is a page that exists and cannot be
    /// opened.
    #[test]
    fn every_tab_is_in_the_tab_strip() {
        // Named individually rather than iterated, because `ALL` is exactly what
        // is under test: a variant added to the enum but forgotten here is a page
        // that exists, compiles, has a label and a body, and can never be opened.
        // `label()`'s `match` is exhaustive, so the compiler covers the naming;
        // nothing but this list covers the reachability.
        #[cfg(feature = "comms")]
        let all = [
            SettingsTab::General,
            SettingsTab::World,
            SettingsTab::Graphics,
            SettingsTab::Audio,
            SettingsTab::Account,
            SettingsTab::Privacy,
            SettingsTab::Tokens,
        ];
        #[cfg(not(feature = "comms"))]
        let all = [
            SettingsTab::General,
            SettingsTab::World,
            SettingsTab::Graphics,
            SettingsTab::Audio,
        ];
        for tab in all {
            assert!(SettingsTab::ALL.contains(&tab), "{tab:?} is not in ALL");
            assert!(!tab.label().is_empty());
        }
    }

    /// Two pages sharing a label would make one of them unreachable in the list
    /// while both still rendered — the sort of thing a copy-pasted arm produces.
    #[test]
    fn tab_labels_are_distinct() {
        let mut seen: Vec<&str> = SettingsTab::ALL.iter().map(|t| t.label()).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), before, "two tabs share a label");
    }

    /// The sky picker's rungs, same contract as the page list: a preference the
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
            "MakeHuman — base mesh, targets, rig, weights, system assets",
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
