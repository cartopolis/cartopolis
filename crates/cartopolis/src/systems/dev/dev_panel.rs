//! Developer tools (Ctrl+Shift+D, or ☰ ▸ Developer tools): what this run is
//! *doing*, as distinct from what it is set to.
//!
//! It lived in `systems::ui` beside the context menu and the prim dialog until it
//! grew tabs, which is the point at which a window with four screens in it stops
//! being a corner of the general UI module.
//!
//! **The distinction between this panel and Settings is what decides where the
//! next control goes**: Settings is where you change the world or what the
//! machine spends on it, and this is where you find out what happened. The clock
//! and the weather went that way (Settings ▸ World, where a phone can reach
//! them); the `CARTO_*` bisecting switches came back this way, because they are
//! not quality knobs — several of them make the picture worse on purpose, and a
//! run with one on is a run whose rendering proves nothing. They sat under
//! Settings ▸ Graphics ▸ Developer, collapsed and last, where the header itself
//! had to apologise for the section being in the wrong panel.
//!
//! **Four tabs**, and the split is by the question being asked:
//!
//! * **Frame** — is it fast, and what is it drawing? Frame timing, the draw and
//!   triangle census by producer, and the two GPU numbers every device-loss
//!   report here has had (a per-frame upload rate and a resident total).
//! * **View** — where is the camera, and how do I reproduce this shot? The
//!   geographic fix, the tier crossfade, the avatar's street address, and the
//!   `[[shot]]` preset the README image set is generated from.
//! * **Profiler** — Bevy's own diagnostics, which this app was registering and
//!   then only ever reading from `cargo perf`. See [`profiler_tab`].
//! * **Switches** — the `CARTO_*` startup switches, on the device.
//!
//! **Nothing in Frame or View invents an instrument.** `FrameStats` is
//! accumulated every frame for the diagnostics report and was legible nowhere;
//! `RenderCensus` was computed on a five-second log timer that is switched off on
//! handsets; `UploadMeter` reached a `slow frame` warning and `--dump-state`;
//! `pipeline_health` reached the shareable report alone. On a workstation all four
//! live in a terminal — which is a fine place for them right up until the question
//! is "is it doing that *now*, while I hold this key down", and then it is not a
//! place at all.
//!
//! **The map's performance overlay is gone** (2026-09-10), and this panel is why.
//! The overlay was a small anchored box of frame rate, frame time and entity
//! count in the top-left corner, switched on from Settings ▸ Graphics — built
//! when the only way to open this panel was a keyboard chord, i.e. never on a
//! phone. That reason expired when Developer tools got a main-menu entry: the
//! Frame tab is reachable in two taps on a handset, says strictly more (worst
//! frame, jitter, triangles by producer, upload and residency), and does not
//! need a second per-frame moving average of its own kept alive to say it.
//!
//! **The library draws it too** (`lib_dev_panel`, `CARTO_UI_LIB=dev_panel`).
//! The readouts are worked out once, as [`DevSection`]s both editions draw;
//! with [`DevTools::lib`] on this system publishes [`DevTools::lib_view`] and
//! does the library's taps through `apply`, the code egui's controls run.
//! Midas too: `midas_link` publishes the tab as `MidasTabView` and does its
//! taps (`MidasAct`) in `MidasLink::apply_tab`, the code egui's
//! `midas_link::midas_tab` runs.

use crate::systems::egui_manager::EguiMut;
use web_time::Instant;

use bevy::diagnostic::DiagnosticsStore;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::systems::icons;
use crate::systems::theme;

/// Which screen of the developer panel is showing.
///
/// A resource rather than a `Local` for the same reason [`crate::systems::settings::SettingsTab`]
/// is one: `--ui profiler` has to be able to name a tab, and a tab that only the
/// pointer can reach is a tab no scripted capture ever photographs.
///
/// Separate from [`crate::resources::DevPanelState`], which holds `open`, because
/// that resource's `is_changed()` is the gate on the day-night lighting recompute
/// — switching a tab would otherwise re-run the sun, the ambient and the
/// atmosphere for a click that changed no light.
#[derive(Resource, Default)]
pub struct DevTools {
    pub tab: DevTab,
    /// The library draws the panel (`lib_dev_panel`, `CARTO_UI_LIB=dev_panel`):
    /// [`render_dev_panel`] draws nothing and publishes
    /// [`lib_view`](Self::lib_view) instead, on every tab.
    pub lib: bool,
    /// What the library's edition shows, while `lib` is on and the panel is
    /// open.
    pub lib_view: Option<DevPanelView>,
    /// Taps on the library's edition, done by [`render_dev_panel`] next
    /// frame (`apply`).
    pub lib_acts: Vec<DevAct>,
    /// When the library's readouts were last handed over (`READOUT_PERIOD_S`).
    pub lib_at: Option<f64>,
}

/// How a readout's value is coloured: a measurement, an absence, a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DevTone {
    Value,
    Dim,
    Error,
}

/// One line of a readout section.
#[derive(Clone, Debug, PartialEq)]
pub enum DevRow {
    /// `name  value` ([`stat`]). A name led by two spaces is a sub-row.
    Stat {
        name: String,
        value: String,
        tone: DevTone,
    },
    /// A monospace line on its own: a failed pipeline's label.
    Mono(String, DevTone),
    /// A quiet explanation (`theme::note`).
    Note(String),
}

/// A titled run of readout rows. Both editions draw the same sections, so
/// the numbers and their wording are worked out once.
#[derive(Clone, Debug, PartialEq)]
pub struct DevSection {
    pub title: &'static str,
    pub rows: Vec<DevRow>,
}

/// One `CARTO_*` switch: its variable, its label, what it does, and whether
/// it is on.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct SwitchRow {
    pub name: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub on: bool,
}

/// The Precision tab, as the library's edition draws it.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct PrecisionView {
    /// What the tracking says, while the walk test runs; `None` before it.
    pub word: Option<crate::systems::high_precision::HandWord>,
    /// The device can run it (a phone).
    pub available: bool,
    pub problem: Option<String>,
    /// "1:05 running · round 2 · 340 field readings".
    pub running: Option<String>,
    pub rounds: u32,
    pub stand_in: bool,
    /// The log's tail; `None` while it is empty.
    pub log: Option<String>,
}

/// What a tab shows in the library's edition.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub enum DevBody {
    /// Frame and Profiler: readouts only.
    Readouts(Vec<DevSection>),
    /// View: the camera's readouts, then the shot preset's group and Copy.
    View {
        sections: Vec<DevSection>,
        group: String,
        can_copy: bool,
        copied: bool,
    },
    Switches {
        on: usize,
        rows: Vec<SwitchRow>,
    },
    Precision(PrecisionView),
    /// Midas in the phone and desktop apps: the link, as
    /// `midas_link::MidasLink::tab_view` has it.
    #[cfg(not(target_arch = "wasm32"))]
    Midas(crate::systems::midas_link::MidasTabView),
    /// A tab with nothing but a line (Midas in the browser).
    #[cfg(target_arch = "wasm32")]
    Note(&'static str),
}

/// What the developer panel shows this frame, published by
/// [`render_dev_panel`] for the library's edition (`lib_dev_panel`).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct DevPanelView {
    pub tab: DevTab,
    pub body: DevBody,
}

/// A tap on the library's edition, done by [`render_dev_panel`].
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub enum DevAct {
    Tab(DevTab),
    Close,
    /// The shot preset's group, as typed.
    Group(String),
    CopyPreset,
    /// A `CARTO_*` switch by its variable, on or off.
    Switch(&'static str, bool),
    /// The walk test: Start, Round, Stop.
    Start,
    Round,
    Stop,
    StandIn(bool),
    CopyLog,
    ClearLog,
    /// A tap on the Midas tab, done by `MidasLink::apply_tab`.
    #[cfg(not(target_arch = "wasm32"))]
    Midas(crate::systems::midas_link::MidasAct),
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum DevTab {
    #[default]
    Frame,
    View,
    Profiler,
    Switches,
    /// The high-precision mode's log (`systems::high_precision`), to copy.
    Precision,
    /// The link to a developer's Midas hub (`systems::midas_link`).
    Midas,
}

impl DevTab {
    pub const ALL: [Self; 6] = [
        Self::Frame,
        Self::View,
        Self::Profiler,
        Self::Switches,
        Self::Precision,
        Self::Midas,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Frame => "Frame",
            Self::View => "View",
            Self::Profiler => "Profiler",
            Self::Switches => "Switches",
            Self::Precision => "Precision",
            Self::Midas => "Midas",
        }
    }
}

/// Toggle the developer panel with Ctrl+Shift+D.
pub fn toggle_dev_panel_shortcut(
    keyboard: Res<ButtonInput<KeyCode>>,
    mut dev_state: ResMut<crate::resources::DevPanelState>,
) {
    let ctrl = keyboard.pressed(KeyCode::ControlLeft) || keyboard.pressed(KeyCode::ControlRight);
    let shift = keyboard.pressed(KeyCode::ShiftLeft) || keyboard.pressed(KeyCode::ShiftRight);
    if ctrl && shift && keyboard.just_pressed(KeyCode::KeyD) {
        dev_state.open = !dev_state.open;
    }
}

/// Where the window opens, clear of the top-left disc column.
///
/// egui's default cascade puts a window near that corner, which is where the
/// layers disc, the compass and the locate button already are. Only a default —
/// the window is draggable and egui remembers where it was put.
pub(crate) const DEV_PANEL_LEFT: f32 = 160.0;

/// The window's width, egui's default and the library's sheet.
pub(crate) const DEV_PANEL_WIDTH: f32 = 300.0;

/// How often the draw/triangle census is re-walked while the panel is open.
///
/// [`crate::systems::render_census::RenderCensus::counts`] resolves every mesh
/// handle in the world, which is a real hitch on a streamed city — that module's
/// note is explicit that it is cheap enough once per capture or once per five
/// seconds and not cheap enough per frame. Twice a second is the compromise a
/// live panel needs: slow enough not to be the thing it is measuring, fast enough
/// that a number moves when the scene does. The frame timing beside it is *not*
/// throttled by this — that comes from [`crate::resources::FrameStats`], which is
/// accumulated every frame regardless.
const CENSUS_PERIOD_S: f64 = 0.5;

/// How often the library's readout rows are rebuilt (`DevTools::lib_at`).
const READOUT_PERIOD_S: f64 = 0.5;

/// The throttled census, kept across frames so the panel is not re-walking the
/// world on every one of them.
#[derive(Default)]
pub struct CensusCache {
    counts: crate::systems::render_census::RenderCounts,
    /// `Time<Real>` seconds at the last walk. `None` until the first.
    at: Option<f64>,
}

/// Everything the Frame and Profiler tabs measure, in one parameter.
///
/// Bevy caps a system at 16 parameters and this one was already at 14 before the
/// tabs arrived — the same ceiling `render_census`'s own note records
/// `drive_capture` hitting.
#[derive(SystemParam)]
pub struct DevReadouts<'w, 's> {
    /// Accumulated every frame by `lib.rs::frame_stats`, whether anything is
    /// reading it or not — so it is honest the instant the panel opens, with no
    /// warm-up and no "0 fps" first second.
    frame: Res<'w, crate::resources::FrameStats>,
    upload: Res<'w, crate::systems::upload_meter::UploadMeter>,
    census: crate::systems::render_census::RenderCensus<'w, 's>,
    diagnostics: Res<'w, DiagnosticsStore>,
    entities: Query<'w, 's, ()>,
}

/// Where the camera is and what is under the avatar.
#[derive(SystemParam)]
pub struct DevView<'w, 's> {
    globe: Res<'w, crate::systems::globe::GlobeState>,
    camera: crate::grid::AnchoredCamera<'w, 's>,
    map_anchor: Res<'w, crate::resources::MapAnchor>,
    /// The avatar's street address, from the BAG. It used to be drawn in the
    /// status readout over the map and nowhere else; that readout is gone
    /// (`systems::hud`'s module note) and this is where it went, because "which
    /// building am I standing at" is the same question as the two rows above it.
    interact: Res<'w, crate::systems::interact::InteractState>,
}

/// Draw the developer panel.
pub fn render_dev_panel(
    mut egui: EguiMut,
    mut dev: ResMut<crate::resources::DevPanelState>,
    mut tools: ResMut<DevTools>,
    mut shot: ResMut<crate::resources::ScreenshotState>,
    mut prefs: ResMut<crate::resources::AppPrefs>,
    time: Res<Time<Real>>,
    readouts: DevReadouts,
    view: DevView,
    mut copied_at: Local<Option<Instant>>,
    mut cache: Local<CensusCache>,
    mut precision_log: ResMut<crate::systems::high_precision::HighPrecisionLog>,
    mut precise_hand: ResMut<crate::systems::prim_hand::PreciseHand>,
    mut high: ResMut<crate::systems::high_precision::HighPrecision>,
    #[cfg(not(target_arch = "wasm32"))] mut midas: ResMut<crate::systems::midas_link::MidasLink>,
    mut commands: Commands,
) {
    // The library's taps of last frame, before anything is measured.
    if tools.lib && !tools.lib_acts.is_empty() {
        let acts = std::mem::take(&mut tools.lib_acts);
        let fix = view.fix();
        for act in acts {
            #[cfg(not(target_arch = "wasm32"))]
            let act = match act {
                DevAct::Midas(a) => {
                    midas.apply_tab(a, &precision_log, &mut commands);
                    continue;
                }
                other => other,
            };
            apply(
                act,
                &mut tools,
                &mut dev,
                &mut shot,
                &mut prefs,
                fix,
                &mut copied_at,
                &mut precision_log,
                &mut precise_hand,
                &mut high,
            );
        }
    }
    if !dev.open {
        if tools.lib_view.is_some() {
            tools.lib_view = None;
        }
        return;
    }
    // Every number in this panel changes on its own, so the window has to be
    // redrawn on its own — without this the readout only updates on the frames
    // something else caused, i.e. it measures the frames *you* caused, which is
    // the opposite of what it is for.
    egui.animating();

    // The census is throttled and the fix is not, but both are measured before
    // the closure so the tab bodies are pure layout — and so the fix is measured
    // once for the two things that read it (the View rows and Copy as preset),
    // which it was not: it used to be re-derived from the same query on every
    // frame the panel was open, for a value only a click consumes.
    let tab = tools.tab;
    let now = time.elapsed_secs_f64();
    let counts = if tab == DevTab::Frame {
        if cache.at.is_none_or(|t| now - t >= CENSUS_PERIOD_S) {
            cache.counts = readouts.census.counts();
            cache.at = Some(now);
        }
        cache.counts
    } else {
        // Not this tab's question, and the walk is the expensive thing in the
        // panel — an open Profiler tab must not cost a full mesh-handle
        // resolution twice a second.
        crate::systems::render_census::RenderCounts::default()
    };
    let fix = view.fix();

    // The library's edition is showing: hand it what to draw, draw nothing.
    if tools.lib {
        // Frame and Profiler numbers change every frame, and a library row
        // rebuilt every frame is never laid out: the tab showed nothing. Twice
        // a second, as the census already is, unless the tab changed.
        let ticking = matches!(tab, DevTab::Frame | DevTab::Profiler);
        let same_tab = tools.lib_view.as_ref().is_some_and(|v| v.tab == tab);
        if ticking && same_tab && tools.lib_at.is_some_and(|t| now - t < READOUT_PERIOD_S) {
            return;
        }
        tools.lib_at = Some(now);
        let body = match tab {
            DevTab::Frame => {
                DevBody::Readouts(frame_sections(&readouts, counts, readouts.entities.count()))
            }
            DevTab::View => DevBody::View {
                sections: camera_sections(fix, view.interact.address.as_deref()),
                group: shot.preset_group.clone(),
                can_copy: fix.is_some(),
                copied: copied_at.is_some_and(|t| t.elapsed().as_secs_f32() < COPIED_S),
            },
            DevTab::Profiler => DevBody::Readouts(profiler_sections(&readouts.diagnostics)),
            DevTab::Switches => {
                let rows: Vec<SwitchRow> = crate::resources::DebugSwitches::SWITCHES
                    .iter()
                    .filter_map(|&(name, label, help)| {
                        Some(SwitchRow {
                            name,
                            label,
                            help,
                            on: prefs.debug_switches.get(name)?,
                        })
                    })
                    .collect();
                DevBody::Switches {
                    on: rows.iter().filter(|r| r.on).count(),
                    rows,
                }
            }
            DevTab::Precision => {
                DevBody::Precision(precision_view(&precision_log, &precise_hand, &high))
            }
            #[cfg(not(target_arch = "wasm32"))]
            DevTab::Midas => DevBody::Midas(midas.tab_view()),
            #[cfg(target_arch = "wasm32")]
            DevTab::Midas => DevBody::Note(MIDAS_IN_THE_APPS),
        };
        let next = DevPanelView { tab, body };
        if tools.lib_view.as_ref() != Some(&next) {
            tools.lib_view = Some(next);
        }
        return;
    }
    if tools.lib_view.is_some() {
        tools.lib_view = None;
    }

    // Edited as a copy and written back below, so an open Switches tab does not
    // mark `AppPrefs` changed on every frame it is open — which would put the
    // preferences autosave on the frame loop. The Settings panel does exactly
    // this, for exactly this reason.
    let mut edited = *prefs;

    let ctx = egui.ctx_mut();
    let mut open = true;
    egui::Window::new("Developer tools")
        .open(&mut open)
        .default_width(DEV_PANEL_WIDTH)
        .default_pos([DEV_PANEL_LEFT + theme::OSD_GAP, theme::OSD_MARGIN])
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.horizontal_wrapped(|ui| {
                for candidate in DevTab::ALL {
                    ui.selectable_value(&mut tools.tab, candidate, candidate.label());
                }
            });
            ui.separator();
            // A scroll area on every tab, not only the long ones: Switches is
            // nine checkboxes each with a paragraph under it, and a window that
            // is taller than a phone has no way to reach its own bottom.
            egui::ScrollArea::vertical()
                .auto_shrink([false, true])
                .max_height(520.0)
                .show(ui, |ui| match tools.tab {
                    DevTab::Frame => {
                        frame_tab(ui, &readouts, counts, readouts.entities.count());
                    }
                    DevTab::View => {
                        view_tab(
                            ui,
                            fix,
                            view.interact.address.as_deref(),
                            &dev,
                            &mut shot,
                            &mut copied_at,
                        );
                    }
                    DevTab::Profiler => profiler_tab(ui, &readouts.diagnostics),
                    DevTab::Switches => switches_tab(ui, &mut edited),
                    DevTab::Precision => {
                        precision_tab(ui, &mut precision_log, &mut precise_hand, &mut high);
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    DevTab::Midas => {
                        crate::systems::midas_link::midas_tab(
                            ui,
                            &mut midas,
                            &precision_log,
                            &mut commands,
                        );
                    }
                    #[cfg(target_arch = "wasm32")]
                    DevTab::Midas => {
                        let _ = &mut commands;
                        theme::note(ui, MIDAS_IN_THE_APPS);
                    }
                });
        });
    if edited != *prefs {
        *prefs = edited;
    }
    // Closing via the window's [x] only writes when it actually changed, to avoid
    // marking the resource changed every frame (which would re-run the lighting
    // update).
    if !open && dev.open {
        dev.open = false;
    }
}

/// What the Midas tab says in the browser.
#[cfg(target_arch = "wasm32")]
const MIDAS_IN_THE_APPS: &str =
    "The link to Midas is in the phone and desktop apps, not the browser.";

/// How long "copied" shows after Copy as preset, s.
const COPIED_S: f32 = 1.5;

/// The shot preset's group field, explained.
pub(crate) const GROUP_HELP: &str = "Subject this viewpoint is for — tunnels, bridges, doors. \
     Captured by `cargo shots --only <group>`, into docs/img/<group>/. \
     Leave blank to add it to the published documentation set.";

/// Copy as preset, explained.
pub(crate) const COPY_HELP: &str = "Copies this camera as a [[shot]] block for docs/shots.toml";

/// Under the View tab.
pub(crate) const CLOCK_NOTE: &str = "The clock and the weather are in Settings, under World.";

/// The indoor walk test's steps, before it starts.
pub(crate) const WALK_STEPS: [&str; 4] = [
    "1. Start outside, under open sky.",
    "2. Hold the phone still, then move it a little, until it says following.",
    "3. Walk one indoor route three times. Tap Round at the start of each.",
    "4. Copy the log below and paste it into the chat.",
];

/// Under a greyed Start.
pub(crate) const NEEDS_A_PHONE: &str =
    "Needs a phone: its back camera, motion sensors and magnetometer.";

/// Under Start.
pub(crate) const WALK_NOTE: &str =
    "Uses the back camera and the motion sensors. Nothing the camera sees \
     is kept. While it runs, things that sense hands near you feel yours.";

/// Under "Stand-in hand".
pub(crate) const STAND_IN_NOTE: &str =
    "Lets your avatar's hand (or the view, in fly mode) play hand-sensing \
     objects such as a theremin before high precision can place your phone. \
     Scripts are told it is not precise.";

/// The precision log, empty.
pub(crate) const LOG_EMPTY_NOTE: &str =
    "Nothing yet. Start the walk test above; what the camera and sensors \
     report is written here every few seconds.";

/// Over the switches, none on.
pub(crate) const SWITCHES_NOTE: &str =
    "Each switch removes a layer, or adds an instrument. All of them are \
     read once, at startup — restart the app to apply a change. An \
     environment variable or a debug.carto.* property still overrides \
     what is set here.";

/// Over the switches, `on` of them on.
pub(crate) fn switches_on(on: usize) -> String {
    format!("{on} on — this run's rendering is not evidence. Restart to apply a change.")
}

/// Do a tap on the library's edition: the same code egui's controls run.
///
/// Each resource is written only when the tap changes it: `DevPanelState`'s
/// `is_changed()` re-runs the lighting, `AppPrefs`' the autosave.
#[allow(clippy::too_many_arguments)]
fn apply(
    act: DevAct,
    tools: &mut ResMut<DevTools>,
    dev: &mut ResMut<crate::resources::DevPanelState>,
    shot: &mut ResMut<crate::resources::ScreenshotState>,
    prefs: &mut ResMut<crate::resources::AppPrefs>,
    fix: Option<ViewFix>,
    copied_at: &mut Option<Instant>,
    log: &mut ResMut<crate::systems::high_precision::HighPrecisionLog>,
    hand: &mut ResMut<crate::systems::prim_hand::PreciseHand>,
    high: &mut ResMut<crate::systems::high_precision::HighPrecision>,
) {
    match act {
        DevAct::Tab(t) => {
            if tools.tab != t {
                tools.tab = t;
            }
        }
        DevAct::Close => {
            if dev.open {
                dev.open = false;
            }
        }
        DevAct::Group(g) => {
            if shot.preset_group != g {
                shot.preset_group = g;
            }
        }
        DevAct::CopyPreset => {
            if let Some(v) = fix {
                copy_preset(v, &shot.preset_group, &**dev, copied_at);
            }
        }
        DevAct::Switch(name, on) => {
            if prefs.debug_switches.get(name).is_some_and(|now| now != on) {
                if let Some(value) = prefs.debug_switches.get_mut(name) {
                    *value = on;
                }
            }
        }
        DevAct::Start => {
            if crate::systems::high_precision::HighPrecision::available() && !high.on {
                high.on = true;
            }
        }
        DevAct::Round => {
            if high.hand_word().is_some() {
                high.mark_round(log);
            }
        }
        DevAct::Stop => {
            if high.on {
                high.on = false;
            }
        }
        DevAct::StandIn(on) => {
            if hand.camera_stands_in != on {
                hand.camera_stands_in = on;
            }
        }
        DevAct::CopyLog => crate::utils::clipboard::set_text(&log.text()),
        DevAct::ClearLog => log.clear(),
        // Done by `MidasLink::apply_tab` before this is called.
        #[cfg(not(target_arch = "wasm32"))]
        DevAct::Midas(_) => {}
    }
}

/// Copy the camera as a `[[shot]]` block, and start the "copied" line.
fn copy_preset(
    v: ViewFix,
    group: &str,
    dev: &crate::resources::DevPanelState,
    copied_at: &mut Option<Instant>,
) {
    crate::utils::clipboard::set_text(&crate::systems::shot_presets::preset_toml(
        "new-view",
        group,
        v.lat,
        v.lng,
        v.alt,
        v.pitch,
        v.yaw,
        dev.time_of_day,
        dev.fog_density,
        dev.cloud_cover,
    ));
    *copied_at = Some(Instant::now());
}

/// The Precision tab's state, for the library's edition.
fn precision_view(
    log: &crate::systems::high_precision::HighPrecisionLog,
    hand: &crate::systems::prim_hand::PreciseHand,
    high: &crate::systems::high_precision::HighPrecision,
) -> PrecisionView {
    let word = high.hand_word();
    let running = word.and(high.running_for()).map(|(ran, readings)| {
        let s = ran.as_secs();
        format!(
            "{}:{:02} running · round {} · {readings} field readings",
            s / 60,
            s % 60,
            high.rounds()
        )
    });
    PrecisionView {
        word,
        available: crate::systems::high_precision::HighPrecision::available(),
        problem: word.and_then(|_| high.problem()),
        running,
        rounds: high.rounds(),
        stand_in: hand.camera_stands_in,
        log: (!log.is_empty()).then(|| log.tail()),
    }
}

/// A `name  value` row in the value's colour.
fn row(name: impl Into<String>, value: impl Into<String>) -> DevRow {
    row_toned(name, value, DevTone::Value)
}

fn row_toned(name: impl Into<String>, value: impl Into<String>, tone: DevTone) -> DevRow {
    DevRow::Stat {
        name: name.into(),
        value: value.into(),
        tone,
    }
}

/// Draw readout sections as egui's panel always has: a header, its rows, and
/// 5 pt between sections.
fn draw_sections(ui: &mut egui::Ui, sections: &[DevSection]) {
    for (i, s) in sections.iter().enumerate() {
        if i > 0 {
            ui.add_space(5.0);
        }
        theme::section(ui, s.title);
        for r in &s.rows {
            match r {
                DevRow::Stat { name, value, tone } => {
                    stat_colored(ui, name, value, tone_color(*tone));
                }
                DevRow::Mono(text, tone) => {
                    ui.label(
                        egui::RichText::new(text)
                            .monospace()
                            .size(theme::SIZE_TINY)
                            .color(tone_color(*tone)),
                    );
                }
                DevRow::Note(text) => theme::note(ui, text),
            }
        }
    }
}

fn tone_color(tone: DevTone) -> egui::Color32 {
    match tone {
        DevTone::Value => theme::p().cream,
        DevTone::Dim => theme::p().text_dim,
        DevTone::Error => theme::p().error,
    }
}

/// Frame: is it fast, and what is it drawing?
fn frame_tab(
    ui: &mut egui::Ui,
    readouts: &DevReadouts,
    counts: crate::systems::render_census::RenderCounts,
    entity_count: usize,
) {
    draw_sections(ui, &frame_sections(readouts, counts, entity_count));
}

/// The Frame tab's readouts.
fn frame_sections(
    readouts: &DevReadouts,
    counts: crate::systems::render_census::RenderCounts,
    entity_count: usize,
) -> Vec<DevSection> {
    let frame = &readouts.frame;
    // Mean, worst and jitter together, because they fail differently: a healthy
    // mean beside a bad worst is a hitch, and a healthy mean beside a high jitter
    // is a pacing problem rather than a load one. A single fps number hides both.
    let timing = if frame.sampled {
        vec![
            row("rate", format!("{} fps", frame.fps)),
            row(
                "frame",
                format!(
                    "{:.1} ms  worst {:.0}  ±{:.1}",
                    frame.mean_ms, frame.worst_ms, frame.jitter_ms
                ),
            ),
        ]
    } else {
        // The window is a whole second wide, so say so rather than print a
        // confident zero — the same call `FrameStats` documents.
        vec![row("rate", "measuring…")]
    };

    let drawn = vec![
        row(
            "draws",
            format!("{}  ·  {} entities", counts.draws, entity_count),
        ),
        row("triangles", thousands(counts.tris_total)),
        // Split by producer, so a regression names the streamer that caused it.
        // Shadow casters are counted apart because each cascade re-draws them — four
        // cascades make that number the one that matters.
        //
        // Both a triangle count and an entity count, because they answer different
        // questions and one of them can be zero while the cost is real: a mesh
        // uploaded `RENDER_WORLD`-only contributes no triangles from this side, and
        // the whole clipmap pyramid is exactly that. The caster count is what shows
        // it.
        row(
            "  shadowed",
            format!(
                "{} ({:.0}%)  ·  {} casters",
                thousands(counts.tris_shadow),
                share(counts.tris_shadow, counts.tris_total),
                thousands(counts.shadow_casters),
            ),
        ),
        row("  buildings", thousands(counts.tris_buildings)),
        row("  3DBAG", thousands(counts.tris_lod22)),
        row("  map tiles", thousands(counts.tris_tiles)),
        row("  globe", thousands(counts.tris_globe)),
    ];

    // The two halves of every device-loss report this project has: a rate (a
    // frame's upload burst times out a command buffer) and a total (streamed
    // content exhausts an allocator). They move independently — the per-frame
    // ceiling flattens the first and does nothing at all about the second — so
    // both are here or neither is worth having. `docs/notes/upload-bursts.md`.
    let upload = &readouts.upload;
    let resident_mb = upload.resident_bytes as f64 / (1024.0 * 1024.0);
    let peak_mb = upload.peak_resident_bytes as f64 / (1024.0 * 1024.0);
    let mut gpu = vec![
        row(
            "upload",
            format!(
                "{} kB now · {} kB peak",
                thousands(upload.frame_kb()),
                thousands(upload.peak_kb()),
            ),
        ),
        row(
            "biggest asset",
            format!("{} kB", thousands(upload.peak_asset_kb())),
        ),
        row(
            "resident",
            format!("{resident_mb:.0} MB · {peak_mb:.0} MB peak"),
        ),
    ];
    // A pipeline that fails to compile is a silent hole in the picture: the
    // render node just skips its pass, and wgpu's one log line about it has long
    // since rolled out of the ring. Named here, live.
    match crate::systems::pipeline_health::snapshot() {
        Some(health) if !health.errored.is_empty() => {
            gpu.push(row_toned(
                "pipelines",
                format!("{} FAILED", health.errored.len()),
                DevTone::Error,
            ));
            for label in &health.errored {
                gpu.push(DevRow::Mono(label.to_string(), DevTone::Error));
            }
        }
        Some(health) => gpu.push(row(
            "pipelines",
            format!("{} ready · {} building", health.ready, health.building),
        )),
        None => gpu.push(row("pipelines", "not measured")),
    }

    vec![
        DevSection {
            title: "Frame",
            rows: timing,
        },
        DevSection {
            title: "Drawn",
            rows: drawn,
        },
        DevSection {
            title: "GPU",
            rows: gpu,
        },
    ]
}

/// The View tab's readouts: the camera and the avatar.
fn camera_sections(fix: Option<ViewFix>, address: Option<&str>) -> Vec<DevSection> {
    // Where the camera is, in the terms the rest of the client argues in: a
    // geographic fix, the altitude that drives every tier decision, and
    // `space_factor` — the 0..1 crossfade the whole space tier rides, which is
    // invisible in a screenshot and is the first thing to check when a ramp looks
    // like a cut.
    let camera = match fix {
        Some(v) => vec![
            row("at", format!("{:.5}, {:.5}", v.lat, v.lng)),
            row("altitude", metres(v.alt)),
            row("look", format!("{:.1}° pitch · {:.1}° yaw", v.pitch, v.yaw)),
            row(
                "tier",
                if v.in_space {
                    format!("space · factor {:.2}", v.space_factor)
                } else {
                    "ground".to_string()
                },
            ),
        ],
        None => vec![row("at", "no camera / no anchor yet")],
    };
    vec![
        DevSection {
            title: "Camera",
            rows: camera,
        },
        // `None` off the Netherlands, away from an address, and in every camera
        // mode but Follow — the register is the only source and it has no
        // opinion about a field in Groningen, let alone one in France.
        DevSection {
            title: "Avatar",
            rows: vec![row("address", address.unwrap_or("nothing near"))],
        },
    ]
}

/// View: where the camera is, and how to reproduce this shot.
fn view_tab(
    ui: &mut egui::Ui,
    fix: Option<ViewFix>,
    address: Option<&str>,
    dev: &crate::resources::DevPanelState,
    shot: &mut crate::resources::ScreenshotState,
    copied_at: &mut Option<Instant>,
) {
    draw_sections(ui, &camera_sections(fix, address));

    ui.add_space(5.0);
    theme::section(ui, "Shot presets");
    // Every remembered camera position lives in `docs/shots.toml` and is captured
    // by `cargo shots`, not by this button — but a *new* position has to be found
    // by flying to it, so this copies the current camera out as a pasteable
    // `[[shot]]` block. That is the whole handoff from "looks good here" to
    // reproducible.
    //
    // **The group is the field worth filling in.** Left empty the preset joins the
    // documentation set, which is published: `cargo shots` regenerates it and the
    // README links every file. Named — `tunnels`, `bridges`, `doors` — it becomes a
    // subject to go and look at, captured by `cargo shots --only <group>` into its
    // own directory, and it stays out of the published set. A viewpoint that
    // clearly shows a tunnel mouth is worth keeping and is not a README image.
    ui.horizontal(|ui| {
        ui.label("group");
        ui.add(
            egui::TextEdit::singleline(&mut shot.preset_group)
                .hint_text("blank = README set")
                .desired_width(120.0),
        )
        .on_hover_text(GROUP_HELP);
    });
    ui.horizontal(|ui| {
        let copied = copied_at.is_some_and(|t| t.elapsed().as_secs_f32() < COPIED_S);
        if ui
            .add_enabled(
                fix.is_some(),
                egui::Button::new(format!("{}  Copy as preset", icons::COPY)),
            )
            .on_hover_text(COPY_HELP)
            .clicked()
        {
            if let Some(v) = fix {
                copy_preset(v, &shot.preset_group, dev, copied_at);
            }
        }
        if copied {
            ui.label("copied");
        }
    });

    theme::note(ui, CLOCK_NOTE);
}

/// Profiler: Bevy's own diagnostics, live.
///
/// **The app was already registering these and reading them from nowhere.**
/// `systems::perf` built its whole report on `DiagnosticsStore` — and only under
/// `cargo perf`, i.e. a scripted flight over five named scenarios on a machine
/// with a keyboard. The engine's measurements were unavailable in the one
/// situation where they are most wanted: something is slow *now*, on this device,
/// doing whatever it is you were doing. This tab is that store, on screen.
///
/// **It invents no plugin either, and the tiering is not a preference.** Three of
/// Bevy's diagnostic plugins are registered in every build because they cost a
/// counter and an atomic (`lib.rs`); the other two are not, for reasons that are
/// each specific:
///
/// * `SystemInformationDiagnosticsPlugin` needs `bevy/sysinfo_plugin`, which
///   pulls the `sysinfo` crate into the binary. That is a build-time cost, so it
///   rides the existing `perf` cargo feature rather than a runtime switch.
/// * `RenderDiagnosticsPlugin` reads GPU timestamp buffers back on a task-pool
///   thread, and on this project's Mesa/Intel host that readback panicked in
///   `Buffer::get_mapped_range` during startup (`frame_pacing`'s note has the
///   measurement). A per-pass timing that can take the app down at launch cannot
///   be on by default on any hardware, so it is behind the `CARTO_GPU_TIMINGS`
///   switch on the tab next door — which is persisted, so a phone can turn it on
///   and relaunch without a cable.
///
/// Absent columns say what would supply them. A profiler that shows an empty
/// table is indistinguishable from a profiler that measured zero, and the whole
/// argument for `--dump-state`'s "not measured" rows applies here unchanged.
fn profiler_tab(ui: &mut egui::Ui, store: &DiagnosticsStore) {
    draw_sections(ui, &profiler_sections(store));
}

/// The Profiler tab's readouts.
fn profiler_sections(store: &DiagnosticsStore) -> Vec<DevSection> {
    use bevy::diagnostic::{EntityCountDiagnosticsPlugin, FrameTimeDiagnosticsPlugin};
    use bevy::render::diagnostic::MeshAllocatorDiagnosticPlugin;

    let read = |path: &bevy::diagnostic::DiagnosticPath| -> Option<f64> {
        store
            .get(path)
            .and_then(bevy::diagnostic::Diagnostic::smoothed)
    };
    // **The process rows are looked up by string, and that is not laziness.**
    // `SystemInformationDiagnosticsPlugin` — the type that owns the constants —
    // is itself behind bevy's `sysinfo_plugin` feature, so naming
    // `SystemInformationDiagnosticsPlugin::PROCESS_CPU_USAGE` would not compile
    // in the builds where these rows are absent, which is every build that is not
    // a profiling one. A path is a string in the store either way; this is the
    // half of the API that survives the feature being off.
    let read_path = |path: &str| -> Option<f64> {
        store
            .iter()
            .find(|d| d.path().as_str() == path)
            .and_then(bevy::diagnostic::Diagnostic::smoothed)
    };
    let measured = |name: &str, value: Option<f64>, unit: &str, decimals: usize| match value {
        Some(v) => row(name, format!("{v:.decimals$} {unit}")),
        None => row_toned(name, "not measured", DevTone::Dim),
    };

    // Bevy's frame timing, beside our own on the Frame tab on purpose: they are
    // measured differently (a rolling smoothed history here, a closed one-second
    // window there) and a disagreement between them is itself informative — it
    // means the last second was not like the seconds before it.
    let engine = vec![
        measured("fps", read(&FrameTimeDiagnosticsPlugin::FPS), "", 1),
        measured(
            "frame time",
            read(&FrameTimeDiagnosticsPlugin::FRAME_TIME),
            "ms",
            2,
        ),
        measured(
            "frame count",
            read(&FrameTimeDiagnosticsPlugin::FRAME_COUNT),
            "",
            0,
        ),
        measured(
            "entities",
            read(&EntityCountDiagnosticsPlugin::ENTITY_COUNT),
            "",
            0,
        ),
    ];

    // The mesh allocator's own view of what it is holding. `UploadMeter` on the
    // Frame tab counts bytes *we* handed over; this counts the slabs bevy packed
    // them into, and the gap between the two is fragmentation.
    let slab_bytes = read(MeshAllocatorDiagnosticPlugin::slabs_size_diagnostic_path());
    let allocator = vec![
        measured(
            "slabs",
            read(MeshAllocatorDiagnosticPlugin::slabs_diagnostic_path()),
            "",
            0,
        ),
        measured(
            "slab bytes",
            slab_bytes.map(|b| b / (1024.0 * 1024.0)),
            "MB",
            1,
        ),
        measured(
            "allocations",
            read(MeshAllocatorDiagnosticPlugin::allocations_diagnostic_path()),
            "",
            0,
        ),
    ];

    let cpu = read_path("process/cpu_usage");
    let mut process = vec![
        measured("cpu", cpu, "% of a core", 1),
        measured("memory", read_path("process/mem_usage"), "% of system", 1),
    ];
    if cpu.is_none() {
        process.push(DevRow::Note(
            "Process CPU and memory need a build with the perf feature: \
             cargo run -p cartopolis --features perf"
                .to_string(),
        ));
    }

    let passes = crate::systems::perf::gpu_passes(store);
    let mut gpu = Vec::new();
    if passes.is_empty() {
        gpu.push(DevRow::Note(
            "No per-pass timings. Switch on \"Time the GPU passes\" under \
             Switches and restart. Vulkan and DX12 only — and some drivers \
             crash on the readback, which is why it is off by default."
                .to_string(),
        ));
    } else {
        for pass in passes.iter().take(12) {
            // The pass name, not the whole path: every one of them begins with
            // the same two segments and a 300 pt window has no room to say so
            // twelve times.
            let name = pass.path.rsplit('/').next().unwrap_or(&pass.path);
            let ms = pass
                .gpu_ms
                .map_or_else(|| "—".to_string(), |ms| format!("{ms:.2} ms"));
            // Fill, in millions of fragment-shader invocations. This is the one
            // GPU-side number that transfers between a phone and a software
            // rasteriser, because it is a count rather than a time.
            let frag = pass
                .fragment_invocations
                .map_or_else(String::new, |n| format!("  ·  {:.1} M frag", n / 1e6));
            gpu.push(row(name, format!("{ms}{frag}")));
        }
        if passes.len() > 12 {
            gpu.push(DevRow::Note(format!(
                "{} more passes — cargo perf writes the full table.",
                passes.len() - 12
            )));
        }
    }

    vec![
        DevSection {
            title: "Engine",
            rows: engine,
        },
        DevSection {
            title: "Mesh allocator",
            rows: allocator,
        },
        DevSection {
            title: "Process",
            rows: process,
        },
        DevSection {
            title: "GPU passes",
            rows: gpu,
        },
    ]
}

/// Switches: the `CARTO_*` bisecting switches, on the device.
///
/// **They were in Settings ▸ Graphics**, collapsed and last, under a header that
/// had to say what they were not — because they are not quality knobs: several of
/// them make the picture worse on purpose, and a run with one on is a run whose
/// rendering proves nothing. A section that has to apologise for the panel it is
/// in belongs in a different panel, and this is it.
///
/// They are on a *device* at all for the reason [`crate::resources::DebugSwitches`]
/// records: on Android the alternative is `debug.carto.*` properties, i.e. a
/// cable, an `adb` and a machine that is not the phone — and the Adreno sky bug
/// spent four days with its own instrument built and unreachable by the person
/// holding the device.
/// The high-precision mode's log: what its sensors reported, as text to copy
/// off the phone and paste into a chat — and, at the top, the indoor walk test
/// (`docs/notes/indoor-positioning.md`) started from here, so it needs no avatar,
/// no walk mode and no hidden button: the hand button only exists on foot.
fn precision_tab(
    ui: &mut egui::Ui,
    log: &mut crate::systems::high_precision::HighPrecisionLog,
    hand: &mut crate::systems::prim_hand::PreciseHand,
    high: &mut crate::systems::high_precision::HighPrecision,
) {
    walk_test(ui, log, high);
    ui.separator();
    theme::checkbox(ui, &mut hand.camera_stands_in, "Stand-in hand");
    theme::note(ui, STAND_IN_NOTE);
    ui.separator();
    if log.is_empty() {
        theme::note(ui, LOG_EMPTY_NOTE);
        return;
    }
    ui.horizontal(|ui| {
        if ui.button("Copy").clicked() {
            crate::utils::clipboard::set_text(&log.text());
        }
        if ui.button("Clear").clicked() {
            log.clear();
        }
    });
    egui::ScrollArea::vertical()
        .max_height(320.0)
        .stick_to_bottom(true)
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(log.tail())
                    .monospace()
                    .size(theme::SIZE_CHIP),
            );
        });
}

/// The indoor walk test: Start, what the tracking needs from you right now, a
/// Round button that marks each pass in the log, and Stop.
fn walk_test(
    ui: &mut egui::Ui,
    log: &mut crate::systems::high_precision::HighPrecisionLog,
    high: &mut crate::systems::high_precision::HighPrecision,
) {
    use crate::systems::high_precision::{HandWord, HighPrecision};
    theme::section(ui, "Indoor walk test");
    let Some(word) = high.hand_word() else {
        for step in WALK_STEPS {
            ui.label(step);
        }
        ui.add_space(theme::OSD_GAP);
        // Drawn on a desktop too, greyed, so a capture shows the test as a
        // phone does.
        let available = HighPrecision::available();
        ui.add_enabled_ui(available, |ui| {
            if theme::primary_button_sized(ui, "Start", 140.0).clicked() {
                high.on = true;
            }
        });
        if !available {
            theme::note(ui, NEEDS_A_PHONE);
        }
        theme::note(ui, WALK_NOTE);
        return;
    };
    let tone = match word {
        HandWord::Following => theme::p().accent_bright,
        HandWord::CameraBusy => theme::p().warn,
        _ => theme::p().text_dim,
    };
    ui.label(
        egui::RichText::new(word.text())
            .size(theme::SIZE_TITLE)
            .strong()
            .color(tone),
    );
    if let Some(problem) = high.problem() {
        ui.label(egui::RichText::new(problem).color(theme::p().warn));
    }
    if let Some((ran, readings)) = high.running_for() {
        let s = ran.as_secs();
        theme::note(
            ui,
            &format!(
                "{}:{:02} running · round {} · {readings} field readings",
                s / 60,
                s % 60,
                high.rounds()
            ),
        );
    }
    ui.add_space(theme::OSD_GAP);
    ui.horizontal(|ui| {
        let next = format!("Round {}", high.rounds() + 1);
        if theme::primary_button_sized(ui, &next, 140.0).clicked() {
            high.mark_round(log);
        }
        if theme::secondary_button(ui, "", "Stop", 100.0).clicked() {
            high.on = false;
        }
    });
}

fn switches_tab(ui: &mut egui::Ui, prefs: &mut crate::resources::AppPrefs) {
    use crate::resources::DebugSwitches;

    let on = DebugSwitches::SWITCHES
        .iter()
        .filter(|(name, _, _)| prefs.debug_switches.get(name) == Some(true))
        .count();
    // The count, at the top, because these are read once at startup and "why does
    // it look like that" has to be answerable without remembering that you set one
    // a week ago.
    if on == 0 {
        theme::note(ui, SWITCHES_NOTE);
    } else {
        ui.label(
            egui::RichText::new(switches_on(on))
                .small()
                .color(theme::p().accent_text),
        );
    }
    ui.add_space(4.0);
    for (name, label, help) in DebugSwitches::SWITCHES {
        let Some(value) = prefs.debug_switches.get_mut(name) else {
            // Unreachable while the table and `get_mut` agree, and a test pins
            // that they do. Skipping beats an `unwrap` in a UI system.
            continue;
        };
        theme::checkbox(ui, value, label);
        ui.label(egui::RichText::new(help).small().color(theme::p().text_dim));
        ui.add_space(4.0);
    }
}

/// A camera fix, measured once per frame and read by both the View rows and the
/// "Copy as preset" button.
#[derive(Clone, Copy)]
struct ViewFix {
    lat: f64,
    lng: f64,
    alt: f64,
    pitch: f32,
    yaw: f32,
    in_space: bool,
    /// The 0..1 crossfade the whole space tier rides. Worth a line of its own
    /// because it is invisible in a screenshot and is the first thing to check
    /// when a ramp — exposure, bloom, atmosphere density, globe albedo — reads
    /// as a hard cut.
    space_factor: f32,
}

impl DevView<'_, '_> {
    fn fix(&self) -> Option<ViewFix> {
        let ((cell, tf), anchor) = self.camera.get().zip(self.map_anchor.geo)?;
        let (lat, lng) = crate::systems::geo_nav::camera_geo(anchor, &self.globe, cell, tf);
        // (yaw, pitch, roll) — the same decomposition free_camera uses to seed its
        // look angles from the live view, so a pasted preset reproduces this exact
        // framing.
        let euler = tf.rotation.to_euler(EulerRot::YXZ);
        Some(ViewFix {
            lat,
            lng,
            alt: self.globe.altitude_m(cell, tf),
            pitch: euler.1.to_degrees(),
            yaw: euler.0.to_degrees(),
            in_space: self.globe.in_space(cell, tf),
            space_factor: self.globe.space_factor(cell, tf),
        })
    }
}

/// One `name  value` row of a readout, with the name in a fixed column so a stack
/// of them lines up and the value monospaced so a changing digit does not shift
/// the ones beside it.
///
/// This is the whole layout vocabulary of the developer panel, deliberately: a
/// panel of numbers wants to be scanned down the left edge, and the moment two
/// rows disagree about where the value starts it stops being scannable.
///
/// The value in a chosen colour: cream for a measurement, another for the
/// rows that report a failure or an absence.
fn stat_colored(ui: &mut egui::Ui, name: &str, value: &str, color: egui::Color32) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [88.0, ui.spacing().interact_size.y],
            egui::Label::new(
                egui::RichText::new(name)
                    .size(theme::SIZE_SUB)
                    .color(theme::p().text_dim),
            ),
        );
        ui.label(
            egui::RichText::new(value)
                .monospace()
                .size(theme::SIZE_MONO)
                .color(color),
        );
    });
}

/// `1234567` → `"1 234 567"`. A thin space rather than a comma because these are
/// counts read at a glance, and six unbroken digits are the reason a triangle
/// budget is hard to eyeball.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}

/// `part` as a percentage of `whole`, and 0 rather than a NaN on an empty scene.
fn share(part: usize, whole: usize) -> f32 {
    if whole == 0 {
        0.0
    } else {
        part as f32 * 100.0 / whole as f32
    }
}

/// An altitude in the unit that suits it: metres near the ground, kilometres once
/// the number stops fitting in a glance.
fn metres(m: f64) -> String {
    if m.abs() >= 10_000.0 {
        format!("{:.0} km", m / 1000.0)
    } else {
        format!("{m:.0} m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The developer panel's number formatting. Small, but these run on every
    /// count in the panel and a grouping that misplaces a separator makes a
    /// triangle budget unreadable, which is the one job the panel has.
    #[test]
    fn counts_group_into_threes_from_the_right() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1\u{202f}000");
        assert_eq!(thousands(12_345), "12\u{202f}345");
        assert_eq!(thousands(1_234_567), "1\u{202f}234\u{202f}567");
    }

    /// An empty scene is the state the panel opens in, so the shadow-share row
    /// must not be a NaN on frame one.
    #[test]
    fn a_share_of_nothing_is_zero_not_a_nan() {
        assert_eq!(share(0, 0), 0.0);
        assert_eq!(share(1, 4), 25.0);
    }

    #[test]
    fn altitude_switches_unit_where_the_digits_stop_fitting() {
        assert_eq!(metres(120.0), "120 m");
        assert_eq!(metres(9_999.0), "9999 m");
        assert_eq!(metres(10_000.0), "10 km");
        assert_eq!(metres(1_200_000.0), "1200 km");
    }

    /// Every tab must be reachable from the tab row, or a screen exists that
    /// nothing can open. Cheap, and it is the failure mode of a hand-written
    /// `ALL` array: the Switches tab arrived by being moved out of another panel,
    /// which is exactly the edit that forgets to list it.
    #[test]
    fn every_tab_is_in_the_tab_row() {
        for tab in [
            DevTab::Frame,
            DevTab::View,
            DevTab::Profiler,
            DevTab::Switches,
            DevTab::Precision,
            DevTab::Midas,
        ] {
            assert!(DevTab::ALL.contains(&tab), "{tab:?} is not in the tab row");
            assert!(!tab.label().is_empty());
        }
    }
}
