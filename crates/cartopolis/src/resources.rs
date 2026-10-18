use crate::platform::sync::{Arc, Mutex, MutexGuard};
use bevy::prelude::*;
use big_space::prelude::GridCell;
use cartopolis_core::{NetMessage, PrimSurface, TextureEntry};
use futures_channel::mpsc::UnboundedSender;
use std::collections::{HashMap, HashSet};

/// Stand-in for the primary `Window` when there isn't one: a scripted `--shot`
/// run is windowless (no winit event loop at all) and renders into an offscreen
/// image, but egui still needs a screen to lay out and scissor against, and the
/// tile/globe LOD heuristics still need a viewport height to size their split
/// thresholds in pixels. Present only in headless runs; every reader falls back
/// to the real `Window` first, so interactive mode is untouched.
#[derive(Resource, Clone, Copy)]
pub struct HeadlessSurface {
    /// Physical pixels of the offscreen target (`--size`).
    pub size: UVec2,
    /// Logical→physical scale. 1.0: there's no monitor to inherit a DPI from,
    /// and the capture is measured in its own pixels.
    pub scale: f32,
}

/// Avatar chooser window (View ▸ Avatar). Edits a `draft` copy of the appearance
/// so dragging the controls doesn't respawn the mesh every frame; Apply commits it
/// to the persisted [`crate::systems::user_store::UserStore`], which the avatar
/// systems watch to rebuild the model.
#[derive(Resource, Default)]
pub struct AvatarChooserState {
    pub open: bool,
    /// Working copy edited by the UI, committed to the store on Apply.
    pub draft: crate::systems::user_store::AvatarAppearance,
    /// Whether `draft` has been seeded from the saved appearance since opening.
    pub seeded: bool,
}

/// Which right-hand panel is open. Exactly one at a time (opening one replaces
/// any other); the bottom bar and the top menu toggle it, and each panel renders
/// with an egui slide-in animation gated on this. `None` = all closed.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq)]
pub enum RightPanel {
    #[default]
    None,
    /// Geo search — place names and pasted coordinates. It was a field in an
    /// address-bar card in the top-left corner until the chrome moved to the
    /// dock; a panel is what a phone can actually type into, since the on-screen
    /// keyboard takes the bottom half of the screen a floating card sat in.
    Search,
    Layers,
    Places,
    Route,
    Chat,
    Appearance,
    Script,
    /// Models: the inventory, the shared library, and putting one in the world.
    Models,
    Settings,
    /// Version, build and the third-party notices. Not a nicety: ODbL
    /// (OpenStreetMap and everything derived from it), CC BY 4.0 (3DBAG, MET
    /// Norway) and the SIL OFL all require the attribution to travel with the
    /// distributed work, and until this existed the app's only credit was three
    /// `ui.small()` lines at the foot of the Layers panel.
    About,
    /// The server's admin pages, for an account whose role grants any of them:
    /// opened from the You page
    /// (`systems::admin`, `docs/mockups/home-and-profile/v2`).
    Admin,
}

impl RightPanel {
    /// Toggle a panel: open it, or close it if it was already the active one.
    pub fn toggle(&mut self, panel: RightPanel) {
        *self = if *self == panel {
            RightPanel::None
        } else {
            panel
        };
    }

    #[must_use]
    pub fn is(self, panel: RightPanel) -> bool {
        self == panel
    }

    /// The panel's name — its dock tooltip, its menu entry and its header title,
    /// from one string so those three can't drift.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Search => "Search",
            Self::Layers => "Map layers",
            Self::Places => "Saved places",
            Self::Route => "Route",
            // Sentence case throughout: these strings are the dock tooltip, the
            // menu entry and the panel header, and "Nearby Chat" beside "Map
            // layers" read as two different applications.
            // "Community", the word the web page's switch uses: the panel
            // holds communities, the public chats and people, not only chats.
            Self::Chat => "Community",
            Self::Appearance => "Appearance",
            Self::Script => "Script",
            Self::Models => "Models",
            Self::Settings => "Settings",
            Self::About => "About",
            Self::Admin => "Admin",
        }
    }

    /// The panel's icon, out of the bundled Material Symbols subset
    /// ([`crate::systems::icons`]).
    ///
    /// This used to be a *preference list* — `["🔍", "⌕", "S"]` — because the
    /// glyph came from whatever egui's font stack happened to cover and half the
    /// obvious choices drew as empty boxes. Shipping the icon font retires that
    /// whole apparatus: the glyph is present by construction, so there is one
    /// answer and no ladder.
    ///
    /// Lives on the enum rather than in the dock because the dock is no longer the
    /// only place a panel's icon appears: the header wears it too (and Appearance
    /// has no dock button at all, only a menu entry).
    #[must_use]
    pub fn icon(self) -> &'static str {
        use crate::systems::icons;
        match self {
            Self::None => " ",
            Self::Search => icons::SEARCH,
            Self::Layers => icons::LAYERS,
            Self::Places => icons::BOOKMARK,
            Self::Route => icons::ROUTE,
            Self::Chat => icons::CHAT,
            Self::Appearance => icons::APPEARANCE,
            Self::Script => icons::SCRIPT,
            // The crate, out of the street-furniture end of the icon set. A
            // model is a thing in a box until you put it somewhere, and the set
            // already carries this glyph — a new one means re-subsetting the
            // font, which is a build step this does not need.
            Self::Models => icons::CABINET,
            Self::Settings => icons::SETTINGS,
            Self::About => icons::ABOUT,
            Self::Admin => icons::TUNE,
        }
    }
}

// ---------------------------------------------------------------------------
// Application preferences (Settings ▸ General / Map)
// ---------------------------------------------------------------------------

/// Which units distances and speeds are shown in.
///
/// Named variants on the wire for the same reason [`SunMode`] has them: adding a
/// unit system must not renumber saved files.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Units {
    /// Metres and kilometres, km/h. The default, and what every formatter in the
    /// client currently hard-codes.
    #[default]
    Metric,
    /// Feet and miles, mph.
    Imperial,
}

impl Units {
    pub const ALL: [Self; 2] = [Self::Metric, Self::Imperial];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Metric => "Metric (km)",
            Self::Imperial => "Imperial (mi)",
        }
    }
}

/// The render budget the user asks for, as opposed to the one
/// [`crate::quality`] detects.
///
/// `Auto` is the detector's answer and is what every install has had until now.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityPreset {
    #[default]
    Auto,
    /// `quality::Budget::phone` — everything that costs fill or main-thread time,
    /// off or halved.
    Low,
    /// `quality::Budget::balanced` — the scene's shape kept, its per-pixel and
    /// per-frame overhead given up.
    Medium,
    /// `quality::Budget::desktop` — the settings the rest of the client is tuned
    /// against.
    High,
}

impl QualityPreset {
    pub const ALL: [Self; 4] = [Self::Auto, Self::Low, Self::Medium, Self::High];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic",
            Self::Low => "Battery saver",
            Self::Medium => "Balanced",
            Self::High => "Full detail",
        }
    }

    /// The word `CARTO_QUALITY` / `?quality=` spells this preset, or `None`
    /// for `Auto` (which is the absence of an override). This is the *whole*
    /// contract with `quality::parse`, and it is deliberately one function: the
    /// vocabulary is duplicated there and in `index.html` already.
    #[must_use]
    pub fn as_env_word(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::Low => Some("phone"),
            Self::Medium => Some("balanced"),
            Self::High => Some("desktop"),
        }
    }
}

/// Which sky the user asks for, as opposed to the one the adapter can draw.
///
/// Three options rather than the developer panel's two checkboxes, because the
/// thing a person wants to choose is *which sky*, and "atmosphere off" plus
/// "force the dome" is that question asked as two booleans with an illegal
/// corner. The rungs are ordered by cost, and every one of them draws a sky —
/// "no sky at all" stays a bisecting switch (`CARTO_NO_ATMOSPHERE`), because a
/// settings screen whose options include a black screen is a bug report waiting
/// to be filed.
///
/// [`Self::Auto`] is the absence of an answer, exactly as [`QualityPreset::Auto`]
/// and `AppPrefs::render_scale = None` are: the adapter is asked whether it can
/// filter a float32 texture and gets the best sky it can draw.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkyPreference {
    #[default]
    Auto,
    /// bevy_pbr's atmosphere — multiple scattering, aerial perspective, a sky
    /// that lights the scene. Refused, with a log line, on an adapter that
    /// cannot sample its LUTs: asking for it cannot make it appear.
    Full,
    /// `systems::sky_dome` — one analytic shell, no compute pass and no LUT.
    /// Chosen automatically where the atmosphere cannot run, and selectable here
    /// because a device that *can* run it may still be better off without it,
    /// and because it is the only way to see what the fallback looks like
    /// without holding the phone that gets it.
    Simple,
}

impl SkyPreference {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Full, Self::Simple];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic",
            Self::Full => "Full atmosphere",
            Self::Simple => "Simple",
        }
    }
}

/// The `CARTO_*` bisecting switches, persisted so that a phone can reach them.
///
/// **This exists because every escape hatch in this codebase is unreachable on
/// the one platform that needs them.** `env_flag`'s own doc says it: a
/// NativeActivity inherits `app_process`'s environment and there is no shell on
/// the device, so the switches arrive as `debug.carto.*` system properties — i.e.
/// through `adb`, i.e. through a cable and a second machine. On 2026-08-23 that
/// was the whole distance between "the Adreno's sky is black" and knowing why:
/// the instrument for it had been built four days earlier and nobody holding the
/// phone could switch it on.
///
/// So the same switches are a *preference* too. The precedent is
/// [`AppPrefs::sky`] and [`AppPrefs::quality`], and the rule is the same one
/// `session::drive_session` states for `--connect`: **an explicit env or property
/// is an instruction for this launch and wins**, and it is never written back;
/// this is what a launch falls back to. `env_flag` resolves the two, so no call
/// site changes and every existing switch becomes reachable at once.
///
/// Named fields rather than a map, because [`AppPrefs`] is `Copy` and because a
/// free-text key is a typo that silently does nothing — the same argument the
/// shot manifest makes for rejecting an unknown key. [`Self::SWITCHES`] is the
/// one table the UI, the lookup and the tests all read.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct DebugSwitches {
    #[serde(default)]
    pub no_atmosphere: bool,
    #[serde(default)]
    pub no_haze: bool,
    #[serde(default)]
    pub no_envmap: bool,
    #[serde(default)]
    pub no_ocean: bool,
    #[serde(default)]
    pub no_dual_source: bool,
    #[serde(default)]
    pub no_space_raymarch: bool,
    #[serde(default)]
    pub gpu_timings: bool,
}

impl DebugSwitches {
    /// Every switch, as `(env name, label, what it proves)`.
    ///
    /// The order is the order a render problem is bisected in: the layers,
    /// outermost first — and last the one that adds an instrument rather than
    /// removing a layer. The help text is written for somebody holding a phone
    /// with no log, so it says what the answer *means* rather than what the
    /// switch does.
    pub const SWITCHES: [(&'static str, &'static str, &'static str); 7] = [
        (
            "CARTO_NO_ATMOSPHERE",
            "No sky at all",
            "Not the simple sky — nothing. A bisecting switch: it says whether \
             what you are looking at is the sky pass or something drawn over it.",
        ),
        (
            "CARTO_NO_HAZE",
            "No horizon haze",
            "Drops our own haze shell. It is a separate thing from the sky, and \
             it is what a black sky's grey band near the horizon usually is.",
        ),
        (
            "CARTO_NO_ENVMAP",
            "No sky reflections",
            "Drops the environment cubemap the atmosphere bakes. Shadowed faces \
             go flat and water goes dark; the sky itself should not move.",
        ),
        (
            "CARTO_NO_OCEAN",
            "No globe ocean",
            "Strips the globe's ocean split — the roughness and tint mask carried \
             in the tile imagery's alpha channel.",
        ),
        (
            "CARTO_NO_DUAL_SOURCE",
            "No dual-source blending",
            "Runs the renderer the way every Adreno already does. On a desktop it \
             reproduces the phone's blend path; on the phone it changes nothing.",
        ),
        (
            "CARTO_NO_SPACE_RAYMARCH",
            "No raymarched sky in space",
            "Keeps the cheap lookup-texture sky from orbit. Costs the atmospheric \
             limb; says whether the raymarch is what a slow orbit view is paying \
             for.",
        ),
        // The one switch here that adds rather than removes, and the one that can
        // take the app down with it: bevy's `RenderDiagnosticsPlugin` reads its
        // GPU timestamp buffers back on a task-pool thread, and on this project's
        // Mesa/Intel host that readback panicked in `Buffer::get_mapped_range`
        // during startup (`frame_pacing`'s note has the measurement). So it is
        // off by default, on any hardware, and the Developer tools' Profiler tab
        // is where it is switched on and where the warning is written.
        (
            "CARTO_GPU_TIMINGS",
            "Time the GPU passes",
            "Adds per-pass GPU and CPU timings to the Profiler tab. Needs Vulkan \
             or DX12 — on the web the columns stay empty, and on Metal the \
             timestamp buffer fails to allocate and stops the renderer. Some \
             drivers crash on the timestamp readback, so if the app dies at \
             startup with this on, that is the answer.",
        ),
    ];

    /// The field behind an env name, or `None` if this switch has no preference.
    ///
    /// Deliberately exhaustive rather than a `_ =>` arm, so adding a field without
    /// adding it here is a compile error rather than a control that does nothing.
    #[must_use]
    pub fn get(&self, env_name: &str) -> Option<bool> {
        Some(match env_name {
            "CARTO_NO_ATMOSPHERE" => self.no_atmosphere,
            "CARTO_NO_HAZE" => self.no_haze,
            "CARTO_NO_ENVMAP" => self.no_envmap,
            "CARTO_NO_OCEAN" => self.no_ocean,
            "CARTO_NO_DUAL_SOURCE" => self.no_dual_source,
            "CARTO_NO_SPACE_RAYMARCH" => self.no_space_raymarch,
            "CARTO_GPU_TIMINGS" => self.gpu_timings,
            _ => return None,
        })
    }

    /// The mutable half of [`Self::get`], for the checkbox that writes it.
    pub fn get_mut(&mut self, env_name: &str) -> Option<&mut bool> {
        Some(match env_name {
            "CARTO_NO_ATMOSPHERE" => &mut self.no_atmosphere,
            "CARTO_NO_HAZE" => &mut self.no_haze,
            "CARTO_NO_ENVMAP" => &mut self.no_envmap,
            "CARTO_NO_OCEAN" => &mut self.no_ocean,
            "CARTO_NO_DUAL_SOURCE" => &mut self.no_dual_source,
            "CARTO_NO_SPACE_RAYMARCH" => &mut self.no_space_raymarch,
            "CARTO_GPU_TIMINGS" => &mut self.gpu_timings,
            _ => return None,
        })
    }

    /// Whether any switch is set, which is what the Settings header reports.
    ///
    /// A run with one of these on is not a run whose rendering means anything, so
    /// it has to be visible without opening the section that set it.
    #[must_use]
    pub fn any(&self) -> bool {
        Self::SWITCHES
            .iter()
            .any(|(name, _, _)| self.get(name) == Some(true))
    }
}

/// The settings that are neither a map layer nor a sky knob: the General, Map,
/// Graphics and Audio tabs of Settings.
///
/// **A resource *and* a serde value.** It is edited as a resource and belongs in
/// `user.json` beside [`crate::systems::user_store::LayerPrefs`] and `SkyPrefs`,
/// which is one line in `UserData` — every leaf carries a default, so adding it
/// cannot fail an existing file's parse (the failure mode that store's module
/// note describes at length).
///
/// Three of these fields have no consumer in the client *yet*, and they say so
/// individually. That is deliberate and is the alternative to faking the effect:
/// a switch that flips a bool nobody reads is a promise to wire it, while a
/// switch that appears to work and does not is a bug report.
#[derive(Resource, Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct AppPrefs {
    /// Distances, speeds and elevations. **No consumer yet**:
    /// `navigation::format_distance` / `format_duration` and the driver bar's
    /// speed pill are metric-hardcoded (`systems/nav/`), so this is the field
    /// they read when they grow a second unit system.
    #[serde(default)]
    pub units: Units,
    /// Hold the screen awake while driver mode is running. **No consumer yet**:
    /// there is no wake-lock seam in `platform` — on the web that is
    /// `navigator.wakeLock`, on Android a `FLAG_KEEP_SCREEN_ON` on the activity
    /// window, and neither exists in this tree.
    #[serde(default = "crate::resources::yes")]
    pub keep_screen_on: bool,
    /// Play the turn chime (and, later, spoken guidance) while navigating.
    /// **Read by the turn-chime work in progress**; harmless while nothing reads
    /// it, since the default is the behaviour the app already had.
    #[serde(default = "crate::resources::yes")]
    pub voice_guidance: bool,
    /// What the user asked the render budget to be. Applied live:
    /// `quality::follow_preset` swaps the budget when this changes, and the
    /// systems that act on it reconcile the scene to the new one
    /// (`quality::select` lists what follows and the little that does not).
    #[serde(default)]
    pub quality: QualityPreset,
    /// The fraction of the screen the 3D world is rendered at, or `None` to follow
    /// the device's tier. A `Resource` the render world re-extracts every frame,
    /// so the picture moves under the slider.
    ///
    /// `None` is "Automatic" — the *absence* of an answer, exactly as
    /// [`QualityPreset::Auto`] is. Clamped on the way in by `RenderScale::new`,
    /// because this file is hand-editable.
    #[serde(default)]
    pub render_scale: Option<f32>,
    /// Level for the synthesised soundscape — footsteps, the landing, the wind
    /// and the driver's turn cues. 1.0 is what the clips are authored at.
    ///
    /// A slider rather than a mute, because the complaint that produced it was
    /// "too loud", not "unwanted": the timbre and the levels were fixed at the
    /// source (`systems::soundscape`) and this is the remaining half a person
    /// can only judge on their own device, with their own speaker, over their
    /// own road noise.
    #[serde(default = "crate::resources::unit_gain")]
    pub effects_volume: f32,
    /// Level for incoming proximity voice. Separate from
    /// [`Self::effects_volume`] because they are turned down for opposite
    /// reasons — the soundscape is atmosphere and voice is people talking, and
    /// the usual want is less of the first and none of the second's loss.
    #[serde(default = "crate::resources::unit_gain")]
    pub voice_volume: f32,
    /// Gain applied to captured microphone audio before it is sent, so a quiet
    /// headset can be lifted without the operating system's mixer. Judged
    /// against the meter in Settings ▸ Audio, which shows the *gained* signal
    /// for that reason.
    #[serde(default = "crate::resources::unit_gain")]
    pub mic_gain: f32,
    /// Which sky to draw. Applied live by `render_features::apply_render_features`,
    /// which resolves it against the adapter into `sky_dome::SkyMode` and puts the
    /// camera, the dome and the render world in step with that one answer.
    #[serde(default)]
    pub sky: SkyPreference,
    /// Frames per second to aim for, or `None` to follow the device's tier.
    /// `Some(0)` is "no cap". Applied live (`frame_pacing::present_rate`); on a
    /// vsync-paced surface it is the rate the display is *asked* for, which the
    /// platform rounds to a mode it has.
    #[serde(default)]
    pub frame_rate: Option<u32>,
    /// Glare around bright light, or `None` to follow the tier. Applied live.
    #[serde(default)]
    pub bloom: Option<bool>,
    /// Multisampled edges, or `None` to follow the tier. Applied live. On is the
    /// tier's own sample count where it has one and 4× where it has none — the only
    /// count every surface supports besides 1 (see `quality::Budget::msaa`).
    #[serde(default)]
    pub anti_aliasing: Option<bool>,
    /// Sun + moon shadow maps, or `None` to follow the device's tier.
    ///
    /// `Option<bool>` rather than a plain `bool`, and the reason is the same one
    /// [`Self::render_scale`] gives: a bare bool written to `user.json` on first
    /// launch would freeze *this* device's tier answer forever, so a later tier
    /// change — or the same file carried to another machine — would be silently
    /// overridden by a preference the user never expressed. `None` is "whatever
    /// this device would have chosen".
    #[serde(default)]
    pub shadows: Option<bool>,
    /// Volumetric fog: sunlight picked out as shafts by the air between you and
    /// whatever is casting. `None` asks the adapter — see
    /// [`Self::light_shafts_on`].
    ///
    /// The most expensive thing in Settings ▸ Graphics and the one most worth
    /// turning off by hand: it is a per-pixel ray march that samples the sun's
    /// shadow cascades at every step, so its cost rides on the shadow settings
    /// above it as well as on the resolution.
    #[serde(default)]
    pub light_shafts: Option<bool>,
    /// Sunbeams fanning out from the sun through the gaps in the clouds and past
    /// the edges of buildings (`systems::sun_rays`). `None` asks the adapter —
    /// see [`Self::sun_rays_on`].
    ///
    /// Separate from [`Self::light_shafts`] because it is a different effect with
    /// a different price: a screen-space pass over the finished frame, one
    /// fixed-length blur toward the sun, needing no shadow map. The shafts are
    /// the air around you cut by the shadow map; these are what the eye expects
    /// of a low sun behind broken cloud, which the shafts cannot draw at all.
    #[serde(default)]
    pub sun_rays: Option<bool>,
    /// Sky-driven image-based lighting: the cubemap that fills shadowed faces and
    /// gives water something to reflect. `None` follows the tier.
    ///
    /// Applied live, and it needs the atmosphere — there is nothing to bake an
    /// environment from on a `SkyMode::Dome` run, which is exactly why
    /// `AMBIENT_DAY_BRIGHTNESS_NO_IBL` exists to stand in for it.
    #[serde(default)]
    pub sky_lighting: Option<bool>,
    /// Hold the connection open while the app is off screen
    /// (`systems::background`, `ConnectionService.java`).
    ///
    /// **Defaults to off, and that is not timidity.** On Android the feature *is*
    /// a permanent notification in the shade — Android obliges a foreground
    /// service to show one — plus a process that no longer gets frozen. Neither
    /// is something to hand somebody who has not asked for it, and the one thing
    /// worse than a phone that misses a message is a phone whose battery went
    /// somewhere the user cannot see. Off until asked for.
    ///
    /// Read on every platform and acted on by exactly one: a desktop keeps its
    /// socket for as long as the process lives, and a wasm client dies with its
    /// tab. `systems::background` documents why the field is not `cfg`-gated to
    /// match — a preference that exists only on some builds is a `user.json` that
    /// loses a setting when it is carried between them.
    #[serde(default)]
    pub background_connection: bool,
    /// The `CARTO_*` bisecting switches. Startup only, like [`Self::sky`], and
    /// overridden by an explicit env var or `debug.carto.*` property — see
    /// [`DebugSwitches`] for why they are persisted at all.
    #[serde(default)]
    pub debug_switches: DebugSwitches,
    /// Serve the Model Context Protocol on loopback, so an editor on this
    /// machine can drive the client (`systems::mcp`).
    ///
    /// **On by default, and switchable here rather than only at launch.** The
    /// thing it enables is an editor attaching to a client that is *already
    /// running* — a flag that had to be passed beforehand asks the person to
    /// have decided before they knew they wanted it, which for the common case
    /// (something is on screen, and now you want to ask an agent about it) is
    /// exactly the wrong moment. So the listener is started and stopped by this
    /// field while the app runs, not by a command line.
    ///
    /// What it costs when on is a loopback socket that refuses everything
    /// without the bearer token in Settings. Read on the desktop and nowhere
    /// else — but not `cfg`-gated, for the reason
    /// [`Self::background_connection`] gives: a preference that exists only on
    /// some builds is a `user.json` that loses a setting when it is carried
    /// between them.
    #[serde(default = "yes")]
    pub mcp_enabled: bool,
}

/// Widest a volume or gain preference may be set to.
///
/// Above 1.0 for [`AppPrefs::mic_gain`] alone — output levels are authored at
/// their intended loudness, and a >1 multiplier there would clip the mix rather
/// than make anything louder.
pub const MAX_MIC_GAIN: f32 = 4.0;

/// serde's `default` for a level that starts at unity.
pub(crate) fn unit_gain() -> f32 {
    1.0
}

/// serde's `default` for a bool that starts on.
pub(crate) fn yes() -> bool {
    true
}

/// Hand-written rather than derived, so `AppPrefs::default()` and a `user.json`
/// with no `settings` block agree. A derived `Default` would give `false` for the
/// two bools while serde gave `true`, i.e. a fresh install and an upgraded one
/// would behave differently — the exact trap `LayerPrefs`' note documents.
impl Default for AppPrefs {
    fn default() -> Self {
        Self {
            units: Units::default(),
            keep_screen_on: yes(),
            voice_guidance: yes(),
            quality: QualityPreset::default(),
            render_scale: None,
            effects_volume: unit_gain(),
            voice_volume: unit_gain(),
            mic_gain: unit_gain(),
            sky: SkyPreference::default(),
            frame_rate: None,
            bloom: None,
            anti_aliasing: None,
            shadows: None,
            light_shafts: None,
            sun_rays: None,
            sky_lighting: None,
            background_connection: false,
            debug_switches: DebugSwitches::default(),
            mcp_enabled: yes(),
        }
    }
}

impl AppPrefs {
    /// Shadows, with `None` resolved against this device's tier.
    ///
    /// The fallback lives here rather than at each call site so that "Automatic"
    /// means one thing: `manage_lights` reads this, the Graphics tab shows this,
    /// and neither can drift into its own idea of what the tier said.
    #[must_use]
    pub fn shadows_on(&self) -> bool {
        self.shadows.unwrap_or(crate::quality::budget().shadows)
    }

    /// Bloom, with `None` resolved against this device's tier. See
    /// [`Self::shadows_on`].
    #[must_use]
    pub fn bloom_on(&self) -> bool {
        self.bloom.unwrap_or(crate::quality::budget().bloom)
    }

    /// Multisampling, with `None` resolved against this device's tier — "on" when
    /// the tier asks for any samples at all. See [`Self::shadows_on`].
    #[must_use]
    pub fn anti_aliasing_on(&self) -> bool {
        self.anti_aliasing
            .unwrap_or(crate::quality::budget().msaa != bevy::render::view::Msaa::Off)
    }

    /// The frame rate to aim for: the preference if one is set, else the tier's.
    /// `0.0` means no cap.
    #[must_use]
    pub fn target_fps(&self) -> f32 {
        match self.frame_rate {
            Some(fps) => fps as f32,
            None => crate::quality::budget().target_fps(),
        }
    }

    /// Sky-driven IBL, with `None` resolved against this device's tier. See
    /// [`Self::shadows_on`].
    #[must_use]
    pub fn sky_lighting_on(&self) -> bool {
        self.sky_lighting
            .unwrap_or(crate::quality::budget().sky_ibl)
    }

    /// Light shafts, with `None` resolved against **what this adapter can do**
    /// rather than against the device's tier.
    ///
    /// This is the difference between this setting and the two above it, and it is
    /// deliberate. `shadows_on` and `sky_lighting_on` fall back to
    /// `quality::budget()`, i.e. to a guess about the *class* of machine, made
    /// before a renderer exists. This falls back to a measured property of the
    /// adapter this run actually found.
    ///
    /// Two refusals, and both are properties of this run rather than judgements
    /// about the picture. A **CPU rasteriser** cannot march this at any usable
    /// rate — measured 2026-09-04, a single scripted capture did not finish in
    /// ten minutes — and unlike every other cost here that is not a matter of
    /// degree. And there is nothing to cut a shaft out of without a **shadow
    /// map**, so with shadows off the effect is a flat wash over the whole frame,
    /// which is worse than none.
    ///
    /// Note both of those bind an explicit `Some(true)` as well: asking for
    /// something the run cannot produce is answered honestly, the way
    /// `SkyPreference::Full` still has to pass the atmosphere's own capability
    /// test.
    ///
    /// One judgement *is* made for `None`, and only for `None`: a main pass too
    /// large to march (`light_shafts::AUTO_MAX_PIXELS`) leaves them off, because
    /// on a Retina panel they were two thirds of the frame. That is a matter of
    /// degree, so an explicit `Some(true)` overrides it.
    #[must_use]
    pub fn light_shafts_on(&self) -> bool {
        let Some(caps) = crate::quality::capabilities() else {
            return false;
        };
        let possible = !caps.software_renderer && self.shadows_on();
        let automatic = possible
            && crate::systems::light_shafts::affordable_at(
                crate::systems::render_scale::main_pass_pixels(),
            );
        self.light_shafts.unwrap_or(automatic) && possible
    }

    /// Sun rays, with `None` resolved against the adapter.
    ///
    /// On by default wherever the GPU is real. Unlike the shafts they need no
    /// shadow map, so shadows do not gate them; a CPU rasteriser is refused
    /// because a full-screen pass of two dozen taps a pixel is seconds a frame
    /// there, and colour is no evidence on one anyway.
    #[must_use]
    pub fn sun_rays_on(&self) -> bool {
        let Some(caps) = crate::quality::capabilities() else {
            return false;
        };
        let possible = !caps.software_renderer;
        self.sun_rays.unwrap_or(possible) && possible
    }

    /// The soundscape level, clamped. `user.json` is hand-editable and a
    /// negative or NaN level would reach `Volume::Linear` unchecked.
    #[must_use]
    pub fn effects_gain(&self) -> f32 {
        clamp_gain(self.effects_volume, 1.0)
    }

    /// The incoming-voice level, clamped. See [`Self::effects_gain`].
    #[must_use]
    pub fn voice_gain(&self) -> f32 {
        clamp_gain(self.voice_volume, 1.0)
    }

    /// The capture gain, clamped to [`MAX_MIC_GAIN`].
    #[must_use]
    pub fn capture_gain(&self) -> f32 {
        clamp_gain(self.mic_gain, MAX_MIC_GAIN)
    }
}

/// NaN-safe clamp into `0..=max`. `f32::clamp` panics on a NaN bound and
/// propagates a NaN value; a level read from a file has to survive both.
fn clamp_gain(v: f32, max: f32) -> f32 {
    if v.is_nan() {
        return 1.0;
    }
    v.clamp(0.0, max)
}

/// How `time_of_day` (the sun/moon/fog clock) is driven — a light/dark/auto
/// switcher like a website's theme toggle. The HUD exposes Auto/Day/Night;
/// Settings ▸ World exposes all four, and its slider, presets and "Run the day"
/// loop all drop into `Manual`.
///
/// Serializable because it is persisted in `data/user.json`
/// ([`crate::systems::user_store::SkyPrefs`]) — the sky comes back the way it
/// was left. Named variants on the wire, so adding a mode never renumbers the
/// saved files.
///
/// `Deserialize` is hand-written (see below) so that an *unknown* name is a
/// fallback rather than an error.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SunMode {
    /// Realistic: the sun follows the real current time at the map location
    /// (local solar time from the anchor longitude — see `utils::sun`).
    #[default]
    Auto,
    /// Forced daylight (noon).
    Day,
    /// Forced night (midnight).
    Night,
    /// Settings ▸ World owns the clock: the slider, the presets and the "Run the
    /// day" loop set `time_of_day` directly and `sky::apply_sun_mode` leaves it
    /// untouched.
    Manual,
}

/// An unrecognised sun mode reads as [`SunMode::Auto`] instead of failing the parse.
///
/// The failure this prevents is disproportionate to its cause: `sun_mode` is one
/// leaf of one optional block of `user.json`, but serde aborts the *whole document*
/// on it, so a user who tried a build with a new mode in it and then rolled back
/// would find every bookmark, their Home and their remembered session gone. Only the
/// sky knob is worth losing there, and losing it means "the sun goes back to Auto",
/// which the next autosave records honestly.
///
/// This is deliberately **not** `#[serde(other)]` and deliberately not a new
/// `Unknown` variant. `#[serde(other)]` is only accepted on internally/adjacently
/// tagged enums, which this — a bare string on disk — is not; and a real variant
/// would have to be handled by every `match` on `SunMode` (the HUD switcher, the dev
/// panel, `apply_sun_mode`), all of which would be answering "what does the sun do in
/// a mode that does not exist?" — a question with no better answer than the default.
impl<'de> serde::Deserialize<'de> for SunMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct NameVisitor;
        impl<'de> serde::de::Visitor<'de> for NameVisitor {
            type Value = SunMode;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a sun mode name (auto, day, night, manual)")
            }

            fn visit_str<E: serde::de::Error>(self, name: &str) -> Result<SunMode, E> {
                Ok(match name {
                    // The names `#[serde(rename_all = "snake_case")]` writes. Spelled
                    // out rather than derived, because this side must keep answering
                    // for names the enum no longer has.
                    "auto" => SunMode::Auto,
                    "day" => SunMode::Day,
                    "night" => SunMode::Night,
                    "manual" => SunMode::Manual,
                    other => {
                        tracing::warn!(mode = %other, "unknown sun mode in user.json; using Auto");
                        SunMode::default()
                    }
                })
            }
        }
        deserializer.deserialize_str(NameVisitor)
    }
}

/// The world's clock and weather, plus whether the developer panel is open.
///
/// **The fields are scene state; the panel that used to own them is not.** Every
/// control over this struct lives in **Settings ▸ World** now — the time of day,
/// the moon, the season and the three weather sliders — because a keyboard chord
/// is not a control on a handset and because "what time is it here" is the least
/// developer-ish question in the client. What stayed behind Ctrl+Shift+D is
/// measurement: frame timing, draw counts, upload and residency, pipeline health,
/// Bevy's own diagnostics, the capture tools, and the `CARTO_*` startup switches
/// — which came back the other way, out of Settings ▸ Graphics, for the mirror of
/// this reason. See [`crate::systems::dev_panel::render_dev_panel`].
///
/// What this struct must *not* grow is a render setting. Those moved to
/// [`AppPrefs`] and Settings ▸ Graphics, and the reason is still live: nothing
/// here is persisted, so a toggle labelled "restart to apply" would revert on the
/// restart meant to apply it.
///
/// `Clone`/`PartialEq` because the Settings panel edits a copy and writes back
/// only on a real change — `ui.slider(&mut res.field)` takes a `DerefMut` and
/// would mark the resource changed on every frame the panel is open, which
/// re-runs the whole day/night lighting recompute.
#[derive(Resource, Clone, PartialEq)]
pub struct DevPanelState {
    pub open: bool,
    /// How the day/night clock is driven (Auto/Day/Night/Manual). `Manual` is
    /// entered automatically by touching the slider, a preset, or Quick-cycle.
    pub sun_mode: SunMode,
    /// Time of day in hours, 0.0–24.0 (12.0 = noon).
    pub time_of_day: f32,
    /// When true, `time_of_day` advances automatically, looping the full 24h.
    pub auto_cycle: bool,
    /// Real seconds for one full day/night loop when `auto_cycle` is on.
    pub cycle_seconds: f32,
    /// Atmospheric distance-fog density, 0.0–1.0: higher pulls the visibility
    /// distance in (see `free_camera::distance_fog`). 0.0 = off.
    pub fog_density: f32,
    /// Cloud coverage, 0.0–1.0: fraction of the sky the procedural cloud deck
    /// fills (see `main::update_clouds`). 0.0 = clear sky.
    pub cloud_cover: f32,
    /// Rain intensity, 0.0–1.0: density of the screen-space rain streaks (see
    /// `main::update_rain`). 0.0 = dry.
    pub precipitation: f32,
    /// Whether either cloud shell is drawn at all — Settings ▸ World ▸ Weather.
    ///
    /// **Not the same question as `cloud_cover == 0.0`, which is why it is its own
    /// field.** Coverage is weather: it is what the met.no poll writes in
    /// Automatic mode, so a user who zeroed the slider gets clouds back the moment
    /// the forecast says cloudy. This is a standing "don't draw them", and it
    /// survives the poll.
    ///
    /// It is also the *only* reachable off-switch for the space-tier shell, whose
    /// coverage is the `sky::GLOBE_CLOUD_COVER` constant blended with live GFS and
    /// never the slider — and `globe_clouds.wesl` evaluates nine 6-octave `fbm3`
    /// fields before it consults coverage, so there is no cheap value to set
    /// either source to. Hiding the shell is the only thing that removes the cost.
    pub clouds_enabled: bool,
    /// Lunar age in days, 0–29.53 (see `utils::sun::LUNAR_CYCLE_DAYS`): 0 = new
    /// moon, ~14.8 = full. Drives the moon's sky position, phase, and light.
    pub moon_age_days: f32,
    /// Solar declination in radians (±0.409 ≈ ±23.44°): the sun's seasonal angle
    /// north/south of the celestial equator. In every automatic mode this rides
    /// the real date (`main::apply_sun_mode`), so the day length, sunrise/sunset
    /// times and peak elevation track the season instead of the old fixed
    /// 06:00-sunrise/18:00-sunset equinox arc. See `utils::sun::sun_direction`.
    pub sun_declination_rad: f32,
    /// The day of the year to light the world for instead of today's: set by a
    /// scripted capture that names a `date`, because a field check re-rendered
    /// later must be lit for the day it was photographed or every shadow moves.
    /// `None` is today.
    pub sun_day_of_year: Option<f32>,
}

// The render feature toggles that used to live here — shadows, atmosphere,
// env-map IBL and the stats overlay — are [`AppPrefs`] fields now, edited in
// Settings ▸ Graphics. They were never developer-only questions: a shadow is
// what a phone gives up first, and the sky is the setting a device with a
// hostile driver most needs to change. Keeping them here also meant they were
// *unpersisted*, so a "restart to apply" toggle reverted on the restart that was
// supposed to apply it.
//
// The clock and the weather followed them out, into Settings ▸ World, for the
// first of those two reasons alone: they are scene state, they belong to
// whoever is looking at the city, and reaching them needed a key chord no phone
// has. They are still *stored* here because `apply_sun_mode`, `advance_day_cycle`
// and `poll_weather` read them from here and because the `is_changed()` gate that
// throttles the lighting recompute is on this resource.

impl Default for DevPanelState {
    fn default() -> Self {
        Self {
            open: false,
            sun_mode: SunMode::Auto,
            time_of_day: 12.0,
            auto_cycle: false,
            cycle_seconds: 10.0,
            fog_density: 0.15,
            cloud_cover: 0.0,
            precipitation: 0.0,
            clouds_enabled: true,
            moon_age_days: 14.8, // full moon
            // Overwritten from the real date on frame 1 by `apply_sun_mode`; this
            // default is the equinox (0°), only ever seen before the first update.
            sun_declination_rad: 0.0,
            sun_day_of_year: None,
        }
    }
}

/// Number of README screenshot slots the dev-panel button cycles through
/// (`docs/img/screenshot-1.png` … `screenshot-3.png`).
pub const SCREENSHOT_SLOTS: usize = 3;

/// Fixed output resolution for captured screenshots (16:9). Every capture is
/// center-cropped to this aspect ratio and resampled to these exact dimensions, so
/// README images stay consistent regardless of the window size at capture time.
pub const SCREENSHOT_WIDTH: u32 = 1920;
pub const SCREENSHOT_HEIGHT: u32 = 1080;

/// Drives the dev-panel "Screenshot" button: on click, egui is suppressed for one
/// frame so the capture has no UI chrome, then a `Screenshot` of the primary window
/// is saved to the next slot. Kept **separate** from [`DevPanelState`] so toggling it
/// every frame doesn't re-trigger the lighting recompute.
#[derive(Resource, Default)]
pub struct ScreenshotState {
    /// Next slot to write (0-based), cycling `0..SCREENSHOT_SLOTS` → files 1..=N.
    pub next_slot: usize,
    /// When true, egui rendering is dropped this frame so the shot has no UI chrome.
    pub hide_ui: bool,
    /// A capture was requested this frame; the driver spawns the `Screenshot` and
    /// clears this on its next run (while `hide_ui` is still active).
    pub capture_requested: bool,
    /// A snapshot of the map for a private chat, by channel key (the messenger's
    /// "Send a snapshot"). Captured the same chrome-free way, and handed to the
    /// chat as a picture rather than saved.
    pub chat_snapshot: Option<String>,
    /// A snapshot of the map for a public post's polaroid
    /// (`community_posts`): captured the same way, framed as a polaroid after.
    pub post_snapshot: bool,
    /// The world half of a field check (`systems::field_check`): captured the
    /// same chrome-free way and handed back to the field check as pixels.
    pub field_check_snapshot: bool,
    /// A polaroid for your own picture (`systems::faces`): the same capture,
    /// handed back for the viewfinder's square to be cut out of it.
    pub face_snapshot: bool,
    /// Group the dev panel's "Copy as preset" writes into the block it copies —
    /// the subject a remembered viewpoint is *for* (`tunnels`, `bridges`,
    /// `doors`). Empty means the published documentation set.
    ///
    /// Session-only, and deliberately not in `AppPrefs`: it is scratch state for
    /// whoever is collecting viewpoints this afternoon, and a remembered value
    /// would quietly file next month's README shot under last month's subject.
    pub preset_group: String,
}

/// The last completed one-second window of frame timing, for the shareable
/// diagnostics report.
///
/// **It is the only frame measurement left.** There used to be a second one — a
/// per-frame exponential moving average written by the map's performance
/// overlay, which existed only while that overlay was being drawn, i.e. never on
/// the handset that most needed it. The overlay is gone (the Developer tools'
/// Frame tab is the readout now) and this is written unconditionally by
/// `lib.rs::frame_stats` from the same accumulator that feeds the `frame stats`
/// log line, so `systems::dev::diagnostics` can report a real frame time in a
/// report the user taps once and sends.
///
/// A phone has no `adb`, no environment variable and no profiler that supports
/// its GPU. The report is the whole instrument, so the numbers have to be in it.
///
/// Zeroed until the first second has elapsed; the report says so rather than
/// printing a confident 0 fps.
#[derive(Resource, Default)]
pub struct FrameStats {
    /// Frames completed in the last whole second.
    pub fps: u32,
    /// Mean frame time over that second, in milliseconds.
    pub mean_ms: f32,
    /// The single worst frame in that second — the number a hitch shows up in,
    /// and the one a mean hides.
    pub worst_ms: f32,
    /// Standard deviation of frame time over the window. A high jitter beside a
    /// healthy mean is the signature of a pacing problem rather than a load one.
    pub jitter_ms: f32,
    /// False until a full second has been accumulated.
    pub sampled: bool,
}

#[derive(Resource, Default)]
pub struct GameState {
    pub selected_prim_id: Option<i64>,
    pub prims_loaded: bool,
    pub editing_prim_id: Option<i64>,
    pub pending_prim_save: Option<EditDialogState>,
    pub pending_prim_revert: Option<EditDialogState>,
    /// Queued prim deletions (processed by send_prim_mutations each frame).
    pub prims_to_delete: Vec<i64>,
}

/// State for rubber-band (marquee) multi-select.
#[derive(Resource, Default)]
pub struct MarqueeState {
    /// Screen position where the mouse was pressed before we know if it's a click or drag.
    pub press_pos: Option<Vec2>,
    /// True while the rubber-band rectangle is being drawn.
    pub active: bool,
    /// Fixed corner (where the drag began).
    pub start: Vec2,
    /// Trailing corner (tracks the cursor).
    pub end: Vec2,
    /// Shift was held at drag start → add to existing selection instead of replacing.
    pub shift_add: bool,
}

/// Texture catalog with decoded egui handles for thumbnail display.
#[derive(Resource, Default)]
pub struct TextureLibrary {
    pub entries: Vec<TextureEntry>,
    /// Registered egui textures (same key as `entries[i].id`) for thumbnail rendering.
    pub egui_handles: HashMap<String, egui::TextureHandle>,
}

/// Client-side cache of decoded texture images, keyed by texture id.
///
/// Textures are fetched **on demand**, never as a catalogue: a prim that wears
/// one, or a picker tile on screen, calls [`Self::want`], and
/// `network::request_wanted_textures` sends a few requests at a time. Fetching
/// the whole catalogue at sign-in (as this once did) uploads every image in
/// one burst — 202 of them hung the GPU five seconds after sign-in.
#[derive(Resource, Default)]
pub struct PrimTextureCache {
    pub handles: HashMap<String, Handle<Image>>,
    /// Requested and not yet answered.
    pub pending: HashSet<String>,
    /// Wanted and not yet requested, oldest first.
    pub queue: std::collections::VecDeque<String>,
}

impl PrimTextureCache {
    /// Ask for a texture. Cheap to call every frame: a texture already held,
    /// on its way or queued is left alone.
    pub fn want(&mut self, id: &str) {
        if !self.handles.contains_key(id)
            && !self.pending.contains(id)
            && !self.queue.iter().any(|q| q == id)
        {
            self.queue.push_back(id.to_string());
        }
    }
}

/// Context menu state for prim interaction (right-click menus)
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct ContextMenuState {
    pub visible: bool,
    pub screen_pos: Vec2,
    pub hit_point: Vec3,
    pub hit_prim_id: Option<i64>,
    /// Where the click landed on the map, in `(lat, lng)`.
    ///
    /// Separate from `hit_point`, which is a *render-space* position under the
    /// floating origin and means nothing outside the frame it was measured in.
    /// The routing entries need a geographic point, so it is resolved once at
    /// click time — the anchor it is relative to can move before the menu is
    /// clicked, and re-deriving it later would place the pin somewhere else.
    /// `None` when the map has no anchor yet (nothing streamed).
    pub lat_lng: Option<(f64, f64)>,
}

/// Twist / taper / top-shear / slice parameters edited together in the dialog.
/// Grouped to avoid duplicating eight fields for both the live and revert copies.
#[derive(Debug, Clone, Copy)]
pub struct PrimWarp {
    pub twist_begin: f32,
    pub twist_end: f32,
    pub taper_x: f32,
    pub taper_y: f32,
    pub top_shear_x: f32,
    pub top_shear_y: f32,
    pub slice_begin: f32,
    pub slice_end: f32,
}

impl Default for PrimWarp {
    fn default() -> Self {
        Self {
            twist_begin: 0.0,
            twist_end: 0.0,
            taper_x: 0.0,
            taper_y: 0.0,
            top_shear_x: 0.0,
            top_shear_y: 0.0,
            slice_begin: 0.0,
            slice_end: 1.0,
        }
    }
}

/// What the edit dialog's Video row asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoAction {
    /// Pick a clip, upload it, and start it on the prim.
    Choose,
    Stop,
}

/// Edit dialog state for prim properties
#[derive(Resource, Debug, Clone)]
pub struct EditDialogState {
    pub visible: bool,
    pub is_new: bool,
    pub prim_id: Option<i64>,
    pub active_tab: u8, // 0 = Basics, 1 = Shape, 2 = Surface
    pub name: String,
    pub shape: String,
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
    pub color: [f32; 3],
    pub texture_id: Option<String>,
    pub texture_picker_open: bool,
    /// Whether a clip is playing on the prim (protocol 28). Shown, not edited:
    /// a clip is started and stopped by its own message, never by the save.
    pub playing_video: bool,
    /// Set by the dialog's Video row; taken by `prim_video::drive_video_actions`.
    pub video_action: Option<VideoAction>,
    /// What the last video action came to, for the row to say.
    pub video_note: Option<String>,
    /// Whether the prim is physical (protocol 34), as the dialog shows it.
    /// Not part of the save — like a video, it has a message of its own.
    pub physical: bool,
    /// Set when the Physical box is ticked or cleared; taken and sent by
    /// `ui::send_prim_mutations`.
    pub physical_request: Option<bool>,
    /// The model asset this prim wears, if it wears one (protocol v22).
    ///
    /// Carried through the dialog even though nothing in the dialog edits it,
    /// which is not optional: `UpdatePrim` sends a prim's whole state, so a field
    /// the dialog forgets is a field the next Apply clears. Taking a model off is
    /// done from the picker, by setting this to `None`.
    pub model: Option<String>,
    pub path_cut_begin: f32,
    pub path_cut_end: f32,
    pub hollow: f32,
    pub warp: PrimWarp,
    /// Texture tab surface params (transparency/glow/full-bright/repeats/rotation/offset).
    pub surface: PrimSurface,
    /// Transient "Repeats Per Meter" editor helper (not persisted); the Apply button
    /// derives `surface.repeat_u/v` from this and the object size.
    pub repeats_per_meter: f32,
    // Snapshot taken when the dialog opens; used to build a revert payload on Cancel.
    pub original_name: String,
    pub original_shape: String,
    pub original_position: [f32; 3],
    pub original_rotation: [f32; 3],
    pub original_scale: [f32; 3],
    pub original_color: [f32; 3],
    pub original_texture_id: Option<String>,
    pub original_model: Option<String>,
    pub original_path_cut_begin: f32,
    pub original_path_cut_end: f32,
    pub original_hollow: f32,
    pub original_warp: PrimWarp,
    pub original_surface: PrimSurface,
}

impl Default for EditDialogState {
    fn default() -> Self {
        Self {
            visible: false,
            is_new: false,
            prim_id: None,
            active_tab: 0,
            name: String::new(),
            shape: String::new(),
            position: [0.0; 3],
            rotation: [0.0; 3],
            scale: [0.0; 3],
            color: [0.0; 3],
            texture_id: None,
            texture_picker_open: false,
            playing_video: false,
            video_action: None,
            video_note: None,
            physical: false,
            physical_request: None,
            model: None,
            path_cut_begin: 0.0,
            path_cut_end: 1.0,
            hollow: 0.0,
            warp: PrimWarp::default(),
            surface: PrimSurface::default(),
            repeats_per_meter: 0.0,
            original_name: String::new(),
            original_shape: String::new(),
            original_position: [0.0; 3],
            original_rotation: [0.0; 3],
            original_scale: [0.0; 3],
            original_color: [0.0; 3],
            original_texture_id: None,
            original_model: None,
            original_path_cut_begin: 0.0,
            original_path_cut_end: 1.0,
            original_hollow: 0.0,
            original_warp: PrimWarp::default(),
            original_surface: PrimSurface::default(),
        }
    }
}

impl EditDialogState {
    /// Geometry deformation params (path-cut / hollow / warp) as a protocol value,
    /// for online `CreatePrim` / `UpdatePrim`.
    pub fn geometry(&self) -> cartopolis_core::PrimGeometry {
        cartopolis_core::PrimGeometry {
            path_cut_begin: self.path_cut_begin,
            path_cut_end: self.path_cut_end,
            hollow: self.hollow,
            twist_begin: self.warp.twist_begin,
            twist_end: self.warp.twist_end,
            taper_x: self.warp.taper_x,
            taper_y: self.warp.taper_y,
            top_shear_x: self.warp.top_shear_x,
            top_shear_y: self.warp.top_shear_y,
            slice_begin: self.warp.slice_begin,
            slice_end: self.warp.slice_end,
        }
    }
}

/// Where the avatar is sitting, while it is (`systems::interact` puts it there,
/// the `sit` clip of the MakeHuman pack supplies the body).
///
/// **A seat is a display offset, not a position.** The sim owns the avatar's
/// translation and has no idea a bench exists — it holds no map data and never
/// will — so moving the avatar onto the seat would be overwritten by the next
/// snapshot, and moving it there *locally* would fight reconciliation for as long
/// as the player sat down. The offset is also not avoidable: the bench is a solid
/// prop ([`crate::systems::world_places`] gives it a circle collider), so the
/// avatar cannot stand where it has to sit. Same shape as the terrain lift, and
/// for the same reason.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seated {
    /// World-XZ from the avatar's own position to the place on the seat, measured
    /// once, when it sat down. Nothing moves while seated, so it does not go
    /// stale.
    pub offset: Vec2,
    /// The world yaw a sitter on this seat turns to — out of the bench, across the
    /// picnic table.
    pub yaw: f32,
    /// How much of the way onto the seat the model has slid, 0..1. Eased rather
    /// than switched, so sitting down and getting up are movements and not cuts;
    /// it rides the same seconds the animation cross-fades over.
    pub blend: f32,
    /// Set the moment the player asks to get up. The seat outlives the decision by
    /// however long the slide takes — clearing it outright would snap the model
    /// back to the avatar's feet in one frame, which is the cut the blend exists to
    /// avoid — so this says "on the way out": the body is already standing and
    /// walking, only the offset is still unwinding.
    pub rising: bool,
}

#[derive(Resource)]
pub struct AvatarState {
    /// Authoritative sim position (from server when online).
    pub position: Vec3,
    /// Visual follow target: smoothed toward `position` online so camera and mesh do not stutter at tick rate.
    ///
    /// This is also where the **terrain** enters (`systems::terrain`): `position` is
    /// the sim's, on the flat plane the sim simulates, and this one stands on the
    /// measured ground. The gap between them is [`Self::terrain_lift`].
    pub display_position: Vec3,
    /// Metres of measured ground currently folded into `display_position` (online
    /// only — offline the terrain is the ground level itself, so the transform
    /// carries it). Stripped before each reconciliation so prediction and
    /// correction are compared on the same flat plane.
    pub terrain_lift: f32,
    /// Metres of *interior floor* under the avatar — which storey of a building it
    /// is standing on (`systems::interiors::InteriorFloors`). Zero outdoors and on
    /// a ground floor, which is every case this client had before buildings had
    /// storeys.
    ///
    /// It enters exactly where [`Self::terrain_lift`] does and for the same
    /// reason: the sim holds no building geometry and never will, so the wire, the
    /// prediction and the reconciliation all stay on the flat plane it actually
    /// simulates and only what you see — and what you walk on — stands on the
    /// storey. The two are separate rather than one summed lift because they are
    /// found differently: the terrain is a field sampled anywhere, and this is the
    /// answer to "may I be raised onto that", which depends on where you already
    /// are.
    pub floor_lift: f32,
    /// Downward speed of `floor_lift` while it is falling, m/s.
    ///
    /// Stepping off a landing has to be a fall rather than a snap, and the
    /// avatar's own `vertical_velocity` cannot do it: in the sim's frame the
    /// avatar is standing on the ground the whole way down, so it is the ground
    /// that has to drop. Under the same gravity, so a storey and a kerb fall at
    /// the same rate.
    pub floor_drop_speed: f32,
    /// The camera's `auto_yaw` when the movement input now held was pressed, and
    /// that input's direction on screen — what keeps a recentring camera from
    /// steering the walk it is following (`avatar::steer_azimuth`). `None` while
    /// nothing is held.
    pub steer_latch: Option<(f32, Vec2)>,
    /// Online client-side prediction: the world-space velocity most recently sent
    /// to the sim (after collision steering + fly scaling), replicating the sim's
    /// own `apply_intent` scaling. `smooth_online_avatar_display` integrates this
    /// at frame rate so movement is smooth between 20 Hz snapshots.
    pub predicted_velocity: Vec3,
    /// The velocity the avatar is *actually* travelling at (m/s, world axes),
    /// written by whichever path owns movement — online it is
    /// [`Self::predicted_velocity`], offline it is what the transform moved by.
    ///
    /// It exists because the follow camera needs it. A first-order lerp trails a
    /// moving target by `speed / rate` metres, which at a walk is a hand's width
    /// and in flight at altitude is tens of metres — past the 25 m the boom can
    /// even reach, so the avatar left the frame and every change of pace slung it
    /// about. `free_camera` leads the rig by exactly that much instead.
    pub velocity: Vec3,
    /// Flight's vertical thrust (m/s, up positive), ramped toward what the climb
    /// keys ask for by [`cartopolis_core::motion::step_fly_vertical`]. Predicted
    /// online, authoritative offline — the same relationship
    /// [`Self::vertical_velocity`] has to the gravity arc.
    pub fly_vertical: f32,
    /// Online prediction: seconds since the last authoritative pose was applied.
    /// The reconcile target is extrapolated forward by this much, so the 20 Hz
    /// snapshot staircase doesn't drag the predicted display backwards on every
    /// frame between snapshots (see `smooth_online_avatar_display`).
    ///
    /// **Uncapped**, and the clamp lives at the one place that extrapolates
    /// (`avatar::extrapolated_target`). It used to be clamped here, which cost the
    /// only measurement of how late a snapshot actually was: the moment the number
    /// is worth reading is the moment a stored clamp stops being able to say it.
    pub snapshot_age: f32,
    /// The avatar's current world yaw (radians) — the single source for the render
    /// transform, the replicated `display_yaw` and the follow-and-rotate camera.
    /// Turned toward the movement direction by `avatar::advance_facing`, so the
    /// avatar faces where it walks (A/D turn it, they don't strafe) instead of
    /// being pinned to camera-forward.
    pub facing_yaw: f32,
    /// Online: replicated world yaw from snapshots (authoritative for reconciliation).
    pub sim_facing_yaw: f32,
    pub is_flying: bool,
    pub is_walking: bool,
    /// Current horizontal speed (m/s), ramped toward the held gait's speed by
    /// `locomotion::advance_speed`. This is the single place the speed model
    /// lands: `handle_avatar_movement` writes it in every run mode (it is the one
    /// movement system that always runs in Follow), the offline path integrates
    /// it, `send_network_intent` turns it into the wish magnitude the sim scales,
    /// and the animation systems read it to match the clip to the ground.
    pub ground_speed: f32,
    /// Horizontal speed (m/s) the avatar is being *carried* at by something other
    /// than its own wish — the locate button walking it from one device fix to the
    /// next (`locate::track_device_pose`). Zero whenever nothing is.
    ///
    /// Separate from [`Self::ground_speed`] because that one is also a *request*:
    /// `send_network_intent` turns it into the wish the sim integrates, so writing
    /// the GPS pace there would have the sim walk the avatar on top of the fix that
    /// already moved it. The body still has to follow the ground it covers — a
    /// GPS walk used to slide an idling avatar down the street — so the clip and
    /// the footsteps read [`Self::stride_speed`], which takes both.
    pub carried_speed: f32,
    /// Unit horizontal direction of travel, **held across a release** so the
    /// avatar coasts to a stop along its own heading instead of stopping dead the
    /// frame the key comes up (the wish direction is zero by then).
    pub move_dir: Vec3,
    /// Gait the held modifiers ask for (Alt stroll / default walk / Shift run).
    pub gait: crate::systems::locomotion::Gait,
    /// Vertical velocity (m/s, up positive) for the jump/fall arc. The sim owns
    /// the authoritative copy; this is the client's prediction of it, and offline
    /// it *is* the avatar's vertical state.
    pub vertical_velocity: f32,
    /// Whether the avatar is standing on the ground. Jump needs an explicit flag
    /// rather than a height test: on a slope the ground moves under the avatar,
    /// and "y == ground_y" is only true on the frames it happens to be exact.
    pub grounded: bool,
    /// Set on the frame Space is pressed on the ground; consumed by whichever
    /// path owns movement (the offline integrator, or `send_network_intent`, which
    /// forwards it to the sim as `ClientIntent::jump`). Never banked: a jump asked
    /// for in mid-air is dropped rather than fired on landing.
    pub wants_jump: bool,
    /// An emote the player triggered this frame, consumed by the same two paths.
    /// The local avatar plays it immediately; online it is also replicated so
    /// everyone else sees it.
    pub pending_emote: Option<cartopolis_core::Emote>,
    /// Downward speed at the moment of the last landing (m/s, positive), set for
    /// one frame. The camera dip and the landing sound read it and it is cleared
    /// on the next frame — a landing is an event, and this is the cheapest way to
    /// publish one without a message type.
    pub landing_impact: f32,
    /// Set when a teleport was sent to the sim (`NetMessage::Teleport`, protocol
    /// 17) and not yet confirmed. Snapshots in flight when the jump was made
    /// still carry the *old* position, and adopting one would yank the avatar
    /// back across the world for a frame or two; `network::adopt_local_pose`
    /// skips the local avatar's pose while this holds and the snapshot is still
    /// far from where the teleport put it. Cleared by the first snapshot that
    /// agrees (the sim's echo), or by timeout — the message can be lost, and an
    /// avatar that ignores authority forever is worse than one that flickers.
    pub teleport_hold: Option<web_time::Instant>,
    /// Bumped by `geo_nav::apply_teleport` every time the avatar is *placed*
    /// somewhere rather than having travelled there.
    ///
    /// A counter rather than a one-frame boolean, for the reason
    /// [`cartopolis_core::EmoteState`] carries one: two jumps to the same place
    /// are two events, and the consumers (the arrival sound, the arrival pose)
    /// run in different phases of the schedule, so a flag cleared by the first
    /// would be invisible to the second. Each keeps the last value it acted on.
    pub teleport_sequence: u32,
    /// Set while the avatar is sitting on a bench or a picnic table. Blocks
    /// locomotion (any movement input stands it back up) and swaps the animation
    /// for the authored seated pose.
    pub seated: Option<Seated>,
}

impl AvatarState {
    /// The speed the legs should move at: the avatar's own pace, or the pace it is
    /// being carried at (`carried_speed`), whichever is faster. What the walk
    /// clip and the footsteps are timed by; never what is sent to the sim.
    #[must_use]
    pub fn stride_speed(&self) -> f32 {
        self.ground_speed.max(self.carried_speed)
    }

    /// Whether the avatar is on a seat right now — which is not the same as
    /// [`Self::seated`] being set, because a seat that is unwinding still is.
    /// This is the one to ask before blocking movement or choosing the pose.
    #[must_use]
    pub fn is_seated(&self) -> bool {
        self.seated.is_some_and(|s| !s.rising)
    }

    /// Ask to stand up, if sitting. Idempotent, and a no-op while already rising.
    pub fn stand_up(&mut self) {
        if let Some(s) = &mut self.seated {
            s.rising = true;
        }
    }

    /// The world-XZ displacement currently applied to the *model* by a seat, or
    /// zero. The camera adds it so the avatar stays framed while it slides onto
    /// the bench, and the model holders apply it so it gets there at all.
    #[must_use]
    pub fn seat_shift(&self) -> Vec3 {
        self.seated.map_or(Vec3::ZERO, |s| {
            let o = s.offset * s.blend;
            Vec3::new(o.x, 0.0, o.y)
        })
    }
}

impl Default for AvatarState {
    fn default() -> Self {
        let p = Vec3::new(0.0, 2.2, 0.0);
        Self {
            position: p,
            display_position: p,
            terrain_lift: 0.0,
            floor_lift: 0.0,
            floor_drop_speed: 0.0,
            steer_latch: None,
            predicted_velocity: Vec3::ZERO,
            velocity: Vec3::ZERO,
            fly_vertical: 0.0,
            snapshot_age: 0.0,
            facing_yaw: 0.0,
            sim_facing_yaw: 0.0,
            is_flying: false,
            is_walking: false,
            ground_speed: 0.0,
            carried_speed: 0.0,
            move_dir: Vec3::ZERO,
            gait: crate::systems::locomotion::Gait::Walk,
            vertical_velocity: 0.0,
            grounded: false,
            wants_jump: false,
            pending_emote: None,
            landing_impact: 0.0,
            teleport_hold: None,
            teleport_sequence: 0,
            seated: None,
        }
    }
}

/// How well the online prediction is being fed — the one instrument for "the
/// avatar stutters online" that a person holding a phone can read.
///
/// **It exists because that report was unanswerable from here.** Prediction
/// smoothness depends on how evenly snapshots reach
/// `avatar::smooth_online_avatar_display`, and until now nothing measured that at
/// all: a stall from a slow mobile link, a stall from our own frame hitch
/// delaying the mailbox drain, and a genuine disagreement with the sim all
/// produced the same jerk on screen and the same silence in the log. The numbers
/// here separate them, and they go into the shareable diagnostics report rather
/// than a `tracing` line for the reason that whole module exists — `logcat` needs
/// a cable, which is exactly what the person with the failing device does not
/// have.
///
/// Written by `avatar::smooth_online_avatar_display` and reset by
/// `session::teardown`, so a figure never describes a connection that has ended.
/// All of it is *observed* time between applied snapshots, which is what the
/// prediction actually rides on — not a round-trip ping, which would say nothing
/// about the client-side half.
#[derive(Resource, Default, Debug)]
pub struct NetPredictionHealth {
    /// Gap between the last two applied snapshots, in seconds. At a 20 Hz server
    /// tick a healthy link sits at ~0.05.
    pub last_gap_s: f32,
    /// The worst gap seen since the session began, in seconds.
    pub worst_gap_s: f32,
    /// Snapshots applied this session.
    pub snapshots: u32,
    /// Frames spent past `avatar::ONLINE_MAX_EXTRAPOLATION`, where the display is
    /// being braked to a halt because there is nothing left to extrapolate from.
    /// **This is the stutter itself**: divided by [`Self::frames`] it is the
    /// fraction of the session the avatar was parked waiting for the network.
    pub stalled_frames: u32,
    /// Frames of online prediction this session, the denominator for
    /// [`Self::stalled_frames`].
    pub frames: u32,
}

impl NetPredictionHealth {
    /// The share of predicted frames that were parked waiting for a snapshot,
    /// 0.0–1.0. Zero when nothing has been predicted yet, which reads correctly as
    /// "no stalls seen".
    #[must_use]
    pub fn stalled_fraction(&self) -> f32 {
        if self.frames == 0 {
            return 0.0;
        }
        self.stalled_frames as f32 / self.frames as f32
    }
}

#[derive(Resource)]
pub struct CameraState {
    pub mode: CameraMode,
    pub distance: f32,
    pub azimuth: f32,
    pub pitch: f32,
    pub pan_offset: Option<Vec2>, // Last mouse position for delta calculation
    pub free_camera_rotation: Vec2, // pitch, yaw
    /// Fly mode's turn about the line of sight, radians: the third angle of the
    /// same `EulerRot::YXZ` rotation. Nothing a person does sets it, only a
    /// scripted capture or a field check, which must match a photograph taken
    /// with the phone held a little crooked. Any jump with a pose puts it back
    /// to level (`CameraPose::apply`).
    pub free_camera_roll: f32,
    /// A vertical field of view, radians, for every mode but Follow (which eases
    /// its own with speed). `None` is the default lens. Like the roll, only a
    /// scripted capture or a field check sets it — `camera_controls` puts the
    /// lens back every frame, so writing the projection directly does nothing —
    /// and any jump with a pose clears it.
    pub free_fov: Option<f32>,
    /// Follow (Avatar) mode is *derived* from the avatar, so it has no saved pose:
    /// cleared on entry so the first frame snaps the rig straight onto the avatar
    /// (reposition reliably even when returning from a distant Orbit `GridCell`),
    /// then the follow lerp takes over.
    pub avatar_initialized: bool,
    /// Fly (Free) mode remembers where you left it. Cleared on entry; the first
    /// frame then restores [`Self::free_saved`] if present (return to the last fly
    /// spot) or seeds the look from the live view (first-ever entry, no jump).
    pub free_initialized: bool,
    /// Last Fly placement — `(GridCell, local translation, pitch/yaw)` — written
    /// every frame in Fly mode so switching to another mode and back returns here
    /// instead of inheriting wherever Orbit/Follow last moved the shared camera.
    pub free_saved: Option<(GridCell, Vec3, Vec2)>,
    /// Fly-mode inertia: the camera's current velocity in local metres per second.
    /// The keys steer *this*, not the translation, so releasing them coasts to a
    /// stop instead of cutting dead. Cleared whenever the camera is placed rather
    /// than flown (mode entry, teleport) — a carried-over velocity would drift the
    /// camera off a fresh placement.
    pub free_velocity: Vec3,
    // ── Earth (orbit) mode ──────────────────────────────────────────────────
    /// Ground point the orbit camera pivots around, in absolute world metres
    /// (the anchored Web Mercator frame `map_stream` uses). y ≈ 0 (the ground).
    pub orbit_focus: bevy::math::DVec3,
    /// Distance from the focus to the camera (metres). The wheel moves
    /// `orbit_distance_target`; this glides toward it each frame, so zoom is a
    /// smooth camera-fly rather than a per-notch jump.
    pub orbit_distance: f64,
    pub orbit_distance_target: f64,
    /// Space tier: the unit direction from the Earth's centre to the camera
    /// (i.e. the sub-camera point on the globe). Drag rotates it.
    pub orbit_dir: bevy::math::DVec3,
    /// Space tier: the camera's up vector, **derived** each frame from the
    /// planet's north axis and [`Self::orbit_heading`] — not integrated through
    /// drags. Kept as state only so a frame where north is degenerate (looking
    /// straight down a pole) can carry the previous up over.
    pub orbit_up: bevy::math::DVec3,
    /// Space tier: view rotation about the view axis, in radians. 0 = north up,
    /// which is where the globe stays unless the user explicitly rotates
    /// (middle-drag / Ctrl+drag) — the Google Earth convention. Holding this as
    /// an explicit angle rather than letting an arcball accumulate roll is what
    /// keeps the horizon level: composed drag rotations do not commute, so an
    /// integrated up vector drifts off north a little more with every gesture.
    pub orbit_heading: f32,
    /// Azimuth around the focus (radians); horizontal drag spins it.
    pub orbit_yaw: f32,
    /// Elevation angle above the ground plane (radians): 0 = grazing the
    /// horizon, π/2 = straight down. Vertical drag tilts it.
    pub orbit_pitch: f32,
    /// Cleared on entering the mode so the first frame seeds the focus/distance
    /// from wherever the camera was looking.
    pub orbit_initialized: bool,
    /// True once Earth mode has ever held a real pose (seeded or restored).
    /// Re-entering Earth restores that pose instead of
    /// reseeding from the current view, so switching away and back returns you to
    /// the same globe location. The first-ever entry (this false) still seeds.
    pub orbit_has_pose: bool,
    /// True once zoomed far enough out that the orbit pivots the Earth's centre
    /// (space tier, ADR-025) instead of a ground focus. Reconstruction is the
    /// same spherical offset; only the pivot and the zoom clamps differ.
    pub orbit_space: bool,
    /// Set by `camera_controls` when the globe descent lands far from the current
    /// anchor: (lat, lng, altitude) to re-anchor the world to. Consumed by
    /// `apply_camera_reanchor`, which turns it into a re-anchoring teleport.
    pub reanchor_request: Option<(f64, f64, f64)>,
    /// Compass request: while true, the Orbit camera eases its view back to
    /// north-up each frame (yaw → 0 on the surface, heading → 0 in space). Set by
    /// the top-right compass overlay, cleared once aligned or as soon as the user
    /// rotates. Ignored outside Orbit (fly/avatar are free-form).
    pub orbit_north_reset: bool,
    /// Compass lock: while true the Orbit camera is *held* north-up instead of
    /// merely eased there once — the view-turn gesture (middle-drag / Ctrl+drag
    /// in space, horizontal rotate on the surface) is ignored and the heading is
    /// re-armed toward north every frame, so the polar axis stays vertical on
    /// screen no matter how the globe is spun. Toggled by clicking the compass.
    pub orbit_north_lock: bool,
    /// Radians the follow camera has turned *by itself* (recentring behind the
    /// avatar), accumulated for the life of the app. Movement reads keys against
    /// the azimuth minus whatever of this accrued since the keys were pressed, so
    /// a camera swinging round behind the avatar does not also steer it
    /// (`avatar::handle_avatar_movement`). A running total rather than a flag,
    /// because the reader and the writer run in different phases of the frame.
    pub auto_yaw: f32,
    /// Follow-and-rotate (Organic-Maps style): while true *and* in Avatar mode, the
    /// follow camera's [`Self::azimuth`] tracks the avatar's facing yaw, so the
    /// camera stays behind the avatar and swings with it like a chase cam. Armed
    /// by Tab, and by the locate button in Avatar mode on a platform with no
    /// compass. Where a compass exists the locate button leaves it alone: the
    /// phone turns the view only from first person
    /// ([`Self::device_rotation`]). Cleared when the user manually
    /// left-drags to orbit (matching OM, where touching the map drops the
    /// rotate-follow).
    pub follow_rotate: bool,
    /// Follow mode, zoomed all the way in: the camera sits *at* the avatar's eyes
    /// instead of on a boom behind them, and the avatar's own model is hidden.
    ///
    /// A sub-state of Avatar mode rather than a fourth [`CameraMode`], because
    /// that is what it is — the far end of the same zoom — and because a variant
    /// would have to be handled by the mode disc, the mode cycle, both saved-pose
    /// paths and every exhaustive `match` in the client for a view that shares all
    /// of its state with the one beside it. It is reached by zooming past
    /// `free_camera::AVATAR_CAM_MIN` and left by any zoom back out; [`Self::distance`]
    /// keeps the third-person distance to return to, so it is not lost.
    pub first_person: bool,
    /// The device is aiming the view: the whole camera rotation, in world space,
    /// as measured by the phone's own sensors.
    ///
    /// Written by [`crate::systems::locate`] while the locate button is lit *and*
    /// [`Self::first_person`] is set, and `None` at every other moment — which is
    /// the identity, so nothing has to branch on whether a phone is aiming the
    /// camera. It is a rotation and not a pair of angles for two reasons: a
    /// quaternion has no gimbal pole to jump through when the phone is pointed
    /// straight up or down, and it carries the roll, which is what keeps a tilted
    /// phone's horizon tilted the same way on screen.
    ///
    /// This is the only way the device's attitude reaches the camera. The third-
    /// person rig used to be turned by the compass bearing too, and magnetometer
    /// noise swung the whole boom about while walking; since 2026-10-07 the phone
    /// turns the view only from inside the head.
    pub device_rotation: Option<Quat>,
    /// Driver mode holds the chase rig itself (`systems::driver`): while this is
    /// set, the Follow camera ignores the wheel and the rotate-drag, and takes its
    /// distance, tilt and azimuth from the map orientation instead. A navigation
    /// view that drifts because the pointer brushed the map is not one.
    pub drive_locked: bool,
    /// World-XZ offset from the avatar to the point the Follow camera *aims* at
    /// while driving. Looking straight at the avatar centres it, and then half the
    /// screen is road already driven; moving the look target forward drops the
    /// marker into the lower third, which is where every navigation device puts it.
    /// Zero outside driver mode, which is also the identity here — nothing has to
    /// branch on the mode to consume it.
    ///
    /// **A vector, not a distance along the camera's forward.** The two are the
    /// same thing in the heading-up views and are not in north-up, where the
    /// camera faces north whatever the vehicle is doing: aiming along the camera
    /// pushed the look point north while driving *south*, so the road ahead ran
    /// off the bottom edge and the marker sat at the very foot of the screen in
    /// every orientation of a north-up drive. Driver mode writes the course
    /// direction here instead (`systems::driver::steady_driver_camera`).
    pub drive_lookahead: Vec2,
    /// Where Follow aims, in metres above the avatar's feet, when something
    /// needs it other than the chest. The avatar designer on a phone aims near
    /// the feet, which tilts the view down and lifts the whole body into the top
    /// half of the screen, clear of its sheet. Only the aim moves, never the
    /// camera, and it is not saved: closing the designer puts it back.
    pub aim_height: Option<f32>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CameraMode {
    Avatar, // Default: camera follows avatar
    Free,   // Free camera mode (FPS-style)
    Orbit,  // "Earth": Google-Earth-style orbit around a ground focus point
}

impl Default for CameraMode {
    fn default() -> Self {
        CameraMode::Avatar
    }
}

impl Default for CameraState {
    fn default() -> Self {
        Self {
            mode: CameraMode::Avatar,
            distance: 6.0,
            azimuth: 0.0,
            pitch: std::f32::consts::PI / 6.0,
            pan_offset: None,
            free_camera_rotation: Vec2::new(0.0, 0.0),
            free_camera_roll: 0.0,
            free_fov: None,
            avatar_initialized: false,
            free_initialized: false,
            free_saved: None,
            free_velocity: Vec3::ZERO,
            orbit_focus: bevy::math::DVec3::ZERO,
            orbit_distance: 1500.0,
            orbit_distance_target: 1500.0,
            orbit_dir: bevy::math::DVec3::Z,
            orbit_up: bevy::math::DVec3::Y,
            orbit_heading: 0.0,
            orbit_yaw: 0.0,
            orbit_pitch: std::f32::consts::FRAC_PI_2 * 0.85,
            orbit_initialized: false,
            orbit_has_pose: false,
            orbit_space: false,
            reanchor_request: None,
            orbit_north_reset: false,
            orbit_north_lock: false,
            auto_yaw: 0.0,
            follow_rotate: false,
            first_person: false,
            device_rotation: None,
            drive_locked: false,
            drive_lookahead: Vec2::ZERO,
            aim_height: None,
        }
    }
}

/// OSM tile URL template (`{z}`/`{x}`/`{y}`); filled from server handshake when online (ADR-014).
#[derive(Resource, Clone)]
pub struct OsmTileUrlTemplate(pub Arc<Mutex<String>>);

impl Default for OsmTileUrlTemplate {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(
            tile_url_override().unwrap_or_default(),
        )))
    }
}

/// `CARTO_TILE_URL` — a `{z}/{x}/{y}` template overriding the tile source, matching
/// the `CARTO_OVERPASS_URL` / `CARTO_3DBAG_URL` convention. This is how the offline
/// and `--shot` paths (which never see a server handshake) reach a non-default
/// source, e.g. our self-hosted VersaTiles vector basemap (the default already):
///
/// ```text
/// CARTO_TILE_URL=https://tiles.cartopolis.org/tiles/osm/{z}/{x}/{y}
/// ```
///
/// Vector vs raster is inferred from the template — see `cartopolis_core::tile_source_kind`.
#[must_use]
pub fn tile_url_override() -> Option<String> {
    std::env::var("CARTO_TILE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// When set, client connects to `simulator` instead of loading local SQLite world.
#[derive(Resource, Clone)]
pub struct ConnectAddr(pub String);

/// Credential presented in the ADR-016 handshake (resolved from `--login`, a login
/// file, env, or the `dev:dev` default). Read by `network::spawn_network_thread`.
#[derive(Resource, Clone)]
pub struct ConnectAuth {
    pub credential: cartopolis_core::AuthCredential,
    /// Persist the issued session token for silent reconnect. True for external
    /// sims only.
    pub persist_token: bool,
}

/// Who is currently authenticated (ADR-016). Set from `ServerHelloAck`; drives the
/// HUD profile chip and, later, chat sender identity.
#[derive(Resource, Clone, Default)]
pub struct AuthIdentity {
    pub username: Option<String>,
    /// The server's name from its handshake (`ServerHelloAck::server_name`,
    /// protocol 37): with `username`, this person's full address, which is what
    /// a private-chat device is made for.
    pub server_name: Option<String>,
    pub principal_id: Option<uuid::Uuid>,
    /// Set when the server confirms this account was deleted at its owner's
    /// request (`NetMessage::AccountDeleted`).
    ///
    /// Here rather than on the Settings panel that asked for it, because the
    /// confirmation arrives on the network system and the disconnect follows
    /// immediately behind it: without somewhere durable to land, "your account is
    /// gone" would be indistinguishable from the connection having dropped, which
    /// is the one reading that would send a person to try signing in again.
    pub deleted: bool,
    /// Why the last account-deletion attempt was refused (wrong password, last
    /// admin). Cleared when the Account tab next asks.
    pub delete_error: Option<String>,
    /// What the server says this account may do, by permission slug
    /// (`NetMessage::Permissions`, resent when an operator changes its roles).
    /// It decides what the client *offers* — the server checks every one itself.
    pub permissions: Vec<String>,
}

impl AuthIdentity {
    /// Whether the signed-in account holds a permission. Always `false` offline.
    #[must_use]
    pub fn may(&self, p: cartopolis_core::permissions::Permission) -> bool {
        self.permissions.iter().any(|s| s == p.slug())
    }
}

/// `request_id` the client stamps on [`cartopolis_core::NetMessage::DeleteAccount`].
///
/// The wire has no general error channel — a post-handshake `ServerError` is
/// otherwise ignored by the client, because nothing knew which request it
/// answered. That is what `request_id` is for, so account deletion picks a
/// recognisable one and the network system routes the refusal back to the panel
/// that asked. Any future request needing an answer should do the same rather
/// than growing a second mechanism.
pub const DELETE_ACCOUNT_REQUEST: u32 = 0x00DE_1E7E;

#[derive(Resource)]
pub struct OnlineSession {
    pub intent_tx: UnboundedSender<NetMessage>,
}

impl OnlineSession {
    /// Send an *authoring* message — one whose loss the user would not notice
    /// until their work is gone (a prim edit, a script save, a chat line, a
    /// block). `unbounded_send` fails exactly when the network thread has
    /// exited, i.e. the link already dropped and the UI hasn't caught up; the
    /// edit was applied to local state and the next authoritative snapshot will
    /// silently revert it. The sim's rate-limit note in `net.rs` makes the same
    /// argument server-side: an author who is told nothing believes their work
    /// was stored. So the failure is logged with what it was, and reported to
    /// the caller for any UI that can say so.
    ///
    /// Per-frame traffic (`ClientIntent`, `VoiceFrame`)
    /// deliberately does not come through here: a dropped movement frame is
    /// corrected by the next one, and a log line per frame during a disconnect
    /// would be noise on top of the failure that matters.
    pub fn send(&self, what: &'static str, msg: NetMessage) -> bool {
        match self.intent_tx.unbounded_send(msg) {
            Ok(()) => true,
            Err(_) => {
                tracing::warn!(
                    what,
                    "not sent: the connection is gone, so this change will not reach the server"
                );
                false
            }
        }
    }
}

/// After the first `WorldSnapshot`, static prims are not torn down each sim tick (ADR-011).
#[derive(Resource, Default)]
pub struct NetworkSyncState {
    pub received_initial_world: bool,
}

/// Set from `ServerHelloAck.your_avatar_id` so we can pick the local row in `WorldSnapshot::avatars`.
#[derive(Resource, Default, Clone, Copy)]
pub struct LocalAvatarSimId(pub Option<u64>);

/// Incoming messages from the network thread (`Receiver` is not `Sync`; wrap in `Mutex`).
#[derive(Resource)]
pub struct NetworkMailbox {
    pub rx: Mutex<crate::platform::sync::mpsc::Receiver<NetMessage>>,
    /// Messages drained so far. `session::drive_session` reads it to tell a link
    /// that has gone quiet from one that is merely idle.
    pub received: std::sync::atomic::AtomicU64,
}

impl NetworkMailbox {
    pub fn new(rx: std::sync::mpsc::Receiver<NetMessage>) -> Self {
        Self {
            rx: Mutex::new(rx),
            received: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn lock_rx(&self) -> MutexGuard<'_, crate::platform::sync::mpsc::Receiver<NetMessage>> {
        self.rx.lock().expect("network mailbox mutex poisoned")
    }
}

/// Where the map is anchored: a (lat, lng) plus base zoom, and a counter that
/// bumps on every re-anchor.
///
/// This is the one thing most of the client wants from the tile streamer. 17 of
/// the 24 modules that took `Res<MapStream>` called nothing on it but
/// `geo_anchor()` — pulling in a resource that also carries a `CellStream`, a
/// `HashMap<TileKey, Entity>`, an `Arc<AtomicU32>` and two asset handles, to read
/// three numbers.
///
/// Splitting it out is not only tidiness. It is what makes the *ordering*
/// expressible: the anchor is written by exactly two systems
/// (`map_stream::init_map_stream` and `map_stream::apply_reanchor`) and read
/// everywhere, but a reader could only order itself against the whole streamer,
/// so almost none did — measured 2026-08-06, 17 of 23 readers had no ordering
/// relation to `apply_reanchor` at all and saw whichever value the frame happened
/// to hold. See `docs/notes/module-graph.md`.
///
/// `MapStream` remains the owner and the only writer; this is its published view.
///
/// Deliberately only the position. The re-anchor *generation* is not mirrored
/// here: every consumer of it (`osm_buildings`, `lod22`, `map_geometry`,
/// `data_layers`, `globe`, `weather`, `navigation`) also needs the streamer
/// itself, so a copy here would have no readers. That is also why the systems
/// below need no ordering against `apply_reanchor` — they compare generations
/// rather than assuming a fresh anchor. See `docs/notes/module-graph.md`.
#[derive(Resource, Default)]
pub struct MapAnchor {
    /// `(lat, lng, base zoom)`. `None` until the first anchor is placed.
    pub geo: Option<(f64, f64, u32)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The table and the fields must not drift apart.** `SWITCHES` is what the
    /// Settings section iterates; `get`/`get_mut` are what the lookup and the
    /// checkbox use. An entry with no field would render a control that silently
    /// does nothing, which is precisely the failure this whole feature exists to
    /// stop happening on a platform with no log.
    #[test]
    fn every_listed_switch_resolves_to_a_field() {
        let mut switches = DebugSwitches::default();
        for (name, label, help) in DebugSwitches::SWITCHES {
            assert!(
                switches.get(name).is_some(),
                "{name} is listed but `get` does not know it"
            );
            assert!(
                switches.get_mut(name).is_some(),
                "{name} is listed but `get_mut` does not know it"
            );
            assert!(!label.is_empty() && !help.is_empty(), "{name} has no text");
        }
    }

    /// The other direction: a field nobody listed is a switch the phone cannot
    /// reach, which is the state this replaced. Counted rather than named,
    /// because the count is what a new field changes.
    #[test]
    fn every_field_is_listed() {
        let json = serde_json::to_value(DebugSwitches::default()).expect("serialises");
        let fields = json.as_object().expect("a struct").len();
        assert_eq!(
            fields,
            DebugSwitches::SWITCHES.len(),
            "add the new switch to `DebugSwitches::SWITCHES` as well as to the struct"
        );
    }

    /// Writing through `get_mut` is what the checkbox does; reading through `get`
    /// is what `env_flag` does. They must see the same bit.
    #[test]
    fn a_switch_written_by_the_checkbox_is_read_by_the_flag() {
        let mut switches = DebugSwitches::default();
        assert!(!switches.any(), "nothing is on by default");
        *switches.get_mut("CARTO_NO_OCEAN").expect("listed") = true;
        assert_eq!(switches.get("CARTO_NO_OCEAN"), Some(true));
        assert_eq!(switches.get("CARTO_NO_HAZE"), Some(false));
        assert!(switches.any(), "the Settings header reports it");
    }

    /// An unknown name is `None`, not `false` — the distinction `env_flag` needs
    /// to tell "this switch has no preference" from "its preference is off".
    #[test]
    fn an_unknown_switch_has_no_preference() {
        assert_eq!(DebugSwitches::default().get("CARTO_TILE_URL"), None);
    }

    /// A `user.json` written before this field existed must still parse, and land
    /// with everything off. The whole `AppPrefs` case is covered below; this pins
    /// the nested object specifically, because it is the one that gained fields.
    #[test]
    fn a_settings_block_without_switches_parses() {
        let prefs: AppPrefs =
            serde_json::from_str(r#"{"units":"metric"}"#).expect("older documents still load");
        assert_eq!(prefs.debug_switches, DebugSwitches::default());
        assert!(!prefs.debug_switches.any());
    }

    /// A fresh install and an upgraded one must land on the same settings. The
    /// two paths are different code — `Default` for the first, serde's per-field
    /// defaults for the second — and they only agree because someone made them.
    #[test]
    fn missing_settings_deserialise_to_the_fresh_install_values() {
        let from_empty: AppPrefs = serde_json::from_str("{}").expect("every leaf is defaulted");
        assert_eq!(from_empty, AppPrefs::default());
        assert!(from_empty.keep_screen_on, "screen-on starts on");
        assert!(from_empty.voice_guidance, "guidance starts on");
    }

    /// The file is the user's only copy of these, so a round trip may not lose
    /// one — and an unknown *name* must not fail the whole document either.
    #[test]
    fn settings_round_trip_through_json() {
        let prefs = AppPrefs {
            units: Units::Imperial,
            keep_screen_on: false,
            voice_guidance: false,
            quality: QualityPreset::Low,
            render_scale: Some(0.75),
            effects_volume: 0.4,
            voice_volume: 0.8,
            mic_gain: 2.0,
            sky: SkyPreference::Simple,
            // `Some(..)`, not `None`: `None` is the serde default, so a field that
            // round-trips only when unset would prove nothing about the field.
            shadows: Some(false),
            // The two capability-resolved ones, set for the same reason: `None`
            // is the serde default and also means "ask the adapter", so a field
            // left unset would round-trip while proving nothing.
            light_shafts: Some(true),
            sun_rays: Some(false),
            sky_lighting: Some(true),
            // `Some(0)` is "no cap" and must survive as itself, not as `None`.
            frame_rate: Some(0),
            bloom: Some(false),
            anti_aliasing: Some(true),
            // Non-default, like everything else here: the field defaults to
            // `false`, so a round trip that left it `false` would pass with the
            // field missing from the document entirely.
            background_connection: true,
            // Set, for the same reason the two `Some(..)` above are: a nested
            // object that round-trips only while it is all-default proves nothing
            // about the nesting.
            debug_switches: DebugSwitches {
                no_ocean: true,
                ..DebugSwitches::default()
            },
            // `false`, and this one defaults to *true*: a round trip that left it
            // true would pass with the field missing from the document, which is
            // exactly the case that would silently re-enable a listener somebody
            // switched off.
            mcp_enabled: false,
        };
        let text = serde_json::to_string(&prefs).expect("serialises");
        assert_eq!(serde_json::from_str::<AppPrefs>(&text).unwrap(), prefs);
    }

    /// The levels are hand-editable and reach `Volume::Linear`, which has no
    /// opinion about a negative or a NaN. A `user.json` someone typed into must
    /// not be able to invert the mix or silence it permanently.
    #[test]
    fn a_hand_edited_level_is_clamped_rather_than_believed() {
        let bad = |v: f32| AppPrefs {
            effects_volume: v,
            voice_volume: v,
            mic_gain: v,
            ..AppPrefs::default()
        };
        for v in [-1.0, 0.0, 0.5, 1.0, 99.0] {
            let p = bad(v);
            assert!((0.0..=1.0).contains(&p.effects_gain()), "effects at {v}");
            assert!((0.0..=1.0).contains(&p.voice_gain()), "voice at {v}");
            assert!(
                (0.0..=MAX_MIC_GAIN).contains(&p.capture_gain()),
                "mic at {v}"
            );
        }
        // NaN falls back to unity rather than propagating into the mix, where it
        // is silence with nothing to say why.
        assert_eq!(bad(f32::NAN).effects_gain(), 1.0);
        assert_eq!(bad(f32::NAN).capture_gain(), 1.0);
    }

    /// `None` means "follow this device", and an explicit answer overrides it in
    /// both directions.
    ///
    /// The direction that matters is `Some(true)` on a tier that says no: a user
    /// asking a battery-saver device for shadows must get them, or the setting is
    /// decoration. Asserted against `quality::budget()` rather than a literal,
    /// because the tier is whatever the machine running the test resolved to and
    /// hard-coding either answer would make this pass or fail by hardware.
    #[test]
    fn an_unset_graphics_preference_follows_the_tier() {
        let tier = crate::quality::budget();
        let auto = AppPrefs::default();
        assert_eq!(
            auto.shadows, None,
            "a fresh install expresses no preference"
        );
        assert_eq!(auto.shadows_on(), tier.shadows);
        assert_eq!(auto.sky_lighting_on(), tier.sky_ibl);
        assert_eq!(auto.bloom_on(), tier.bloom);
        assert_eq!(
            auto.anti_aliasing_on(),
            tier.msaa != bevy::render::view::Msaa::Off
        );
        assert_eq!(auto.target_fps(), tier.target_fps());

        for answer in [true, false] {
            let set = AppPrefs {
                shadows: Some(answer),
                sky_lighting: Some(answer),
                bloom: Some(answer),
                anti_aliasing: Some(answer),
                ..AppPrefs::default()
            };
            assert_eq!(set.shadows_on(), answer, "shadows overridden to {answer}");
            assert_eq!(
                set.sky_lighting_on(),
                answer,
                "sky lighting overridden to {answer}"
            );
            assert_eq!(set.bloom_on(), answer, "bloom overridden to {answer}");
            assert_eq!(
                set.anti_aliasing_on(),
                answer,
                "anti-aliasing overridden to {answer}"
            );
        }
        for fps in [0, 30, 120] {
            let set = AppPrefs {
                frame_rate: Some(fps),
                ..AppPrefs::default()
            };
            assert_eq!(
                set.target_fps(),
                fps as f32,
                "a chosen rate wins, 0 included"
            );
        }
    }

    /// A `user.json` written before the Graphics tab existed must keep working,
    /// and must not have a tier decision frozen into it by the upgrade.
    #[test]
    fn an_older_settings_file_gains_the_graphics_defaults() {
        let old = r#"{"units":"imperial","quality":"low","effects_volume":0.5}"#;
        let prefs: AppPrefs = serde_json::from_str(old).expect("older files still parse");
        assert_eq!(prefs.units, Units::Imperial, "the old fields survive");
        assert_eq!(prefs.quality, QualityPreset::Low);
        assert_eq!(prefs.sky, SkyPreference::Auto);
        assert_eq!(prefs.shadows, None);
        assert_eq!(prefs.sky_lighting, None);
        assert_eq!(prefs.frame_rate, None);
        assert_eq!(prefs.bloom, None);
        assert_eq!(prefs.anti_aliasing, None);
    }

    /// A file written before the levels existed must not arrive silent.
    #[test]
    fn levels_missing_from_an_older_file_default_to_unity() {
        let old: AppPrefs =
            serde_json::from_str(r#"{"units":"metric","keep_screen_on":true}"#).expect("parses");
        assert_eq!(old.effects_gain(), 1.0);
        assert_eq!(old.voice_gain(), 1.0);
        assert_eq!(old.capture_gain(), 1.0);
    }

    /// The quality preset's only contract with `crate::quality` is the word it
    /// spells, and that vocabulary is duplicated in `quality::parse` and in
    /// `index.html`. A word only one side knows puts the canvas resolution and
    /// the renderer's settings in different tiers, and nothing reports that.
    #[test]
    fn the_quality_preset_speaks_the_tier_vocabulary() {
        assert_eq!(QualityPreset::Auto.as_env_word(), None);
        assert_eq!(QualityPreset::Low.as_env_word(), Some("phone"));
        assert_eq!(QualityPreset::Medium.as_env_word(), Some("balanced"));
        assert_eq!(QualityPreset::High.as_env_word(), Some("desktop"));
    }
}
