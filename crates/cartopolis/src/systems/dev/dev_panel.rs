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

use web_time::Instant;

use bevy::diagnostic::DiagnosticsStore;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::systems::egui_manager::EguiManager;
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
}

impl DevTab {
    pub const ALL: [Self; 5] = [
        Self::Frame,
        Self::View,
        Self::Profiler,
        Self::Switches,
        Self::Precision,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Frame => "Frame",
            Self::View => "View",
            Self::Profiler => "Profiler",
            Self::Switches => "Switches",
            Self::Precision => "Precision",
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
const DEV_PANEL_LEFT: f32 = 160.0;

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
    mut egui: ResMut<EguiManager>,
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
) {
    if !dev.open {
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
    // Edited as a copy and written back below, so an open Switches tab does not
    // mark `AppPrefs` changed on every frame it is open — which would put the
    // preferences autosave on the frame loop. The Settings panel does exactly
    // this, for exactly this reason.
    let mut edited = *prefs;

    let ctx = egui.ctx_mut();
    let mut open = true;
    egui::Window::new("Developer tools")
        .open(&mut open)
        .default_width(300.0)
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
                    DevTab::Precision => precision_tab(ui, &mut precision_log),
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

/// Frame: is it fast, and what is it drawing?
fn frame_tab(
    ui: &mut egui::Ui,
    readouts: &DevReadouts,
    counts: crate::systems::render_census::RenderCounts,
    entity_count: usize,
) {
    let frame = &readouts.frame;
    // Mean, worst and jitter together, because they fail differently: a healthy
    // mean beside a bad worst is a hitch, and a healthy mean beside a high jitter
    // is a pacing problem rather than a load one. A single fps number hides both.
    theme::section(ui, "Frame");
    if frame.sampled {
        stat(ui, "rate", &format!("{} fps", frame.fps));
        stat(
            ui,
            "frame",
            &format!(
                "{:.1} ms  worst {:.0}  ±{:.1}",
                frame.mean_ms, frame.worst_ms, frame.jitter_ms
            ),
        );
    } else {
        // The window is a whole second wide, so say so rather than print a
        // confident zero — the same call `FrameStats` documents.
        stat(ui, "rate", "measuring…");
    }

    ui.add_space(5.0);
    theme::section(ui, "Drawn");
    stat(
        ui,
        "draws",
        &format!("{}  ·  {} entities", counts.draws, entity_count),
    );
    stat(ui, "triangles", &thousands(counts.tris_total));
    // Split by producer, so a regression names the streamer that caused it.
    // Shadow casters are counted apart because each cascade re-draws them — four
    // cascades make that number the one that matters.
    //
    // Both a triangle count and an entity count, because they answer different
    // questions and one of them can be zero while the cost is real: a mesh
    // uploaded `RENDER_WORLD`-only contributes no triangles from this side, and
    // the whole clipmap pyramid is exactly that. The caster count is what shows
    // it.
    stat(
        ui,
        "  shadowed",
        &format!(
            "{} ({:.0}%)  ·  {} casters",
            thousands(counts.tris_shadow),
            share(counts.tris_shadow, counts.tris_total),
            thousands(counts.shadow_casters),
        ),
    );
    stat(ui, "  buildings", &thousands(counts.tris_buildings));
    stat(ui, "  3DBAG", &thousands(counts.tris_lod22));
    stat(ui, "  map tiles", &thousands(counts.tris_tiles));
    stat(ui, "  globe", &thousands(counts.tris_globe));

    ui.add_space(5.0);
    // The two halves of every device-loss report this project has: a rate (a
    // frame's upload burst times out a command buffer) and a total (streamed
    // content exhausts an allocator). They move independently — the per-frame
    // ceiling flattens the first and does nothing at all about the second — so
    // both are here or neither is worth having. `docs/notes/upload-bursts.md`.
    theme::section(ui, "GPU");
    let upload = &readouts.upload;
    stat(
        ui,
        "upload",
        &format!(
            "{} kB now · {} kB peak",
            thousands(upload.frame_kb()),
            thousands(upload.peak_kb()),
        ),
    );
    stat(
        ui,
        "biggest asset",
        &format!("{} kB", thousands(upload.peak_asset_kb())),
    );
    let resident_mb = upload.resident_bytes as f64 / (1024.0 * 1024.0);
    let peak_mb = upload.peak_resident_bytes as f64 / (1024.0 * 1024.0);
    stat(
        ui,
        "resident",
        &format!("{resident_mb:.0} MB · {peak_mb:.0} MB peak"),
    );
    // A pipeline that fails to compile is a silent hole in the picture: the
    // render node just skips its pass, and wgpu's one log line about it has long
    // since rolled out of the ring. Named here, live.
    match crate::systems::pipeline_health::snapshot() {
        Some(health) if !health.errored.is_empty() => {
            stat_colored(
                ui,
                "pipelines",
                &format!("{} FAILED", health.errored.len()),
                theme::ERROR,
            );
            for label in &health.errored {
                ui.label(
                    egui::RichText::new(label)
                        .monospace()
                        .size(theme::SIZE_TINY)
                        .color(theme::ERROR),
                );
            }
        }
        Some(health) => stat(
            ui,
            "pipelines",
            &format!("{} ready · {} building", health.ready, health.building),
        ),
        None => stat(ui, "pipelines", "not measured"),
    }
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
    // Where the camera is, in the terms the rest of the client argues in: a
    // geographic fix, the altitude that drives every tier decision, and
    // `space_factor` — the 0..1 crossfade the whole space tier rides, which is
    // invisible in a screenshot and is the first thing to check when a ramp looks
    // like a cut.
    theme::section(ui, "Camera");
    match fix {
        Some(v) => {
            stat(ui, "at", &format!("{:.5}, {:.5}", v.lat, v.lng));
            stat(ui, "altitude", &metres(v.alt));
            stat(
                ui,
                "look",
                &format!("{:.1}° pitch · {:.1}° yaw", v.pitch, v.yaw),
            );
            stat(
                ui,
                "tier",
                &if v.in_space {
                    format!("space · factor {:.2}", v.space_factor)
                } else {
                    "ground".to_string()
                },
            );
        }
        None => stat(ui, "at", "no camera / no anchor yet"),
    }

    ui.add_space(5.0);
    theme::section(ui, "Avatar");
    // `None` off the Netherlands, away from an address, and in every camera mode
    // but Follow — the register is the only source and it has no opinion about a
    // field in Groningen, let alone one in France.
    stat(ui, "address", address.unwrap_or("nothing near"));

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
        .on_hover_text(
            "Subject this viewpoint is for — tunnels, bridges, doors. \
             Captured by `cargo shots --only <group>`, into docs/img/<group>/. \
             Leave blank to add it to the published documentation set.",
        );
    });
    ui.horizontal(|ui| {
        let copied = copied_at.is_some_and(|t| t.elapsed().as_secs_f32() < 1.5);
        if ui
            .add_enabled(
                fix.is_some(),
                egui::Button::new(format!("{}  Copy as preset", icons::COPY)),
            )
            .on_hover_text("Copies this camera as a [[shot]] block for docs/shots.toml")
            .clicked()
        {
            if let Some(v) = fix {
                crate::utils::clipboard::set_text(&crate::systems::shot_presets::preset_toml(
                    "new-view",
                    &shot.preset_group,
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
        }
        if copied {
            ui.label("copied");
        }
    });
    ui.horizontal(|ui| {
        // Clicking captures a chrome-free shot into the next slot, cycling
        // docs/img/screenshot-{1..N}.png. The `capture_requested` guard stops a
        // held click from queuing more than one capture.
        if ui
            .button(format!(
                "{}  Capture slot {}",
                icons::CAMERA,
                shot.next_slot + 1
            ))
            .on_hover_text("Hides the UI for one frame, then saves docs/img/screenshot-N.png")
            .clicked()
            && !shot.hide_ui
        {
            shot.hide_ui = true;
            shot.capture_requested = true;
        }
        if ui.button("Reset to 1").clicked() {
            shot.next_slot = 0;
        }
    });

    theme::note(
        ui,
        "The clock and the weather are in Settings, under World.",
    );
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
    let row = |ui: &mut egui::Ui, name: &str, value: Option<f64>, unit: &str, decimals: usize| {
        match value {
            Some(v) => stat(ui, name, &format!("{v:.decimals$} {unit}")),
            None => stat_colored(ui, name, "not measured", theme::TEXT_DIM),
        }
    };

    // Bevy's frame timing, beside our own on the Frame tab on purpose: they are
    // measured differently (a rolling smoothed history here, a closed one-second
    // window there) and a disagreement between them is itself informative — it
    // means the last second was not like the seconds before it.
    theme::section(ui, "Engine");
    row(ui, "fps", read(&FrameTimeDiagnosticsPlugin::FPS), "", 1);
    row(
        ui,
        "frame time",
        read(&FrameTimeDiagnosticsPlugin::FRAME_TIME),
        "ms",
        2,
    );
    row(
        ui,
        "frame count",
        read(&FrameTimeDiagnosticsPlugin::FRAME_COUNT),
        "",
        0,
    );
    row(
        ui,
        "entities",
        read(&EntityCountDiagnosticsPlugin::ENTITY_COUNT),
        "",
        0,
    );

    ui.add_space(5.0);
    // The mesh allocator's own view of what it is holding. `UploadMeter` on the
    // Frame tab counts bytes *we* handed over; this counts the slabs bevy packed
    // them into, and the gap between the two is fragmentation.
    theme::section(ui, "Mesh allocator");
    row(
        ui,
        "slabs",
        read(MeshAllocatorDiagnosticPlugin::slabs_diagnostic_path()),
        "",
        0,
    );
    let slab_bytes = read(MeshAllocatorDiagnosticPlugin::slabs_size_diagnostic_path());
    row(
        ui,
        "slab bytes",
        slab_bytes.map(|b| b / (1024.0 * 1024.0)),
        "MB",
        1,
    );
    row(
        ui,
        "allocations",
        read(MeshAllocatorDiagnosticPlugin::allocations_diagnostic_path()),
        "",
        0,
    );

    ui.add_space(5.0);
    theme::section(ui, "Process");
    let cpu = read_path("process/cpu_usage");
    row(ui, "cpu", cpu, "% of a core", 1);
    row(
        ui,
        "memory",
        read_path("process/mem_usage"),
        "% of system",
        1,
    );
    if cpu.is_none() {
        theme::note(
            ui,
            "Process CPU and memory need a build with the perf feature: \
             cargo run -p cartopolis --features perf",
        );
    }

    ui.add_space(5.0);
    theme::section(ui, "GPU passes");
    let passes = crate::systems::perf::gpu_passes(store);
    if passes.is_empty() {
        theme::note(
            ui,
            "No per-pass timings. Switch on \"Time the GPU passes\" under \
             Switches and restart. Vulkan and DX12 only — and some drivers \
             crash on the readback, which is why it is off by default.",
        );
    } else {
        for pass in passes.iter().take(12) {
            // The pass name, not the whole path: every one of them begins with
            // the same two segments and a 300 pt window has no room to say so
            // twelve times.
            let name = pass.path.rsplit('/').next().unwrap_or(&pass.path);
            let gpu = pass
                .gpu_ms
                .map_or_else(|| "—".to_string(), |ms| format!("{ms:.2} ms"));
            // Fill, in millions of fragment-shader invocations. This is the one
            // GPU-side number that transfers between a phone and a software
            // rasteriser, because it is a count rather than a time.
            let frag = pass
                .fragment_invocations
                .map_or_else(String::new, |n| format!("  ·  {:.1} M frag", n / 1e6));
            stat(ui, name, &format!("{gpu}{frag}"));
        }
        if passes.len() > 12 {
            theme::note(
                ui,
                &format!(
                    "{} more passes — cargo perf writes the full table.",
                    passes.len() - 12
                ),
            );
        }
    }
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
/// off the phone and paste into a chat. Long-press the locate button ▸ High
/// precision to fill it.
fn precision_tab(ui: &mut egui::Ui, log: &mut crate::systems::high_precision::HighPrecisionLog) {
    if log.is_empty() {
        theme::note(
            ui,
            "Nothing yet. Long-press the locate button and switch on High precision; \
             what the camera and sensors report is written here every few seconds.",
        );
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
                egui::RichText::new(log.text())
                    .monospace()
                    .size(theme::SIZE_CHIP),
            );
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
        theme::note(
            ui,
            "Each switch removes a layer, or adds an instrument. All of them are \
             read once, at startup — restart the app to apply a change. An \
             environment variable or a debug.carto.* property still overrides \
             what is set here.",
        );
    } else {
        ui.label(
            egui::RichText::new(format!(
                "{on} on — this run's rendering is not evidence. Restart to apply a change."
            ))
            .small()
            .color(theme::ACCENT_TEXT),
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
        ui.label(egui::RichText::new(help).small().color(theme::TEXT_DIM));
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
fn stat(ui: &mut egui::Ui, name: &str, value: &str) {
    stat_colored(ui, name, value, theme::CREAM);
}

/// [`stat`] with the value in a chosen colour — for the rows that report a
/// failure or an absence rather than a measurement.
fn stat_colored(ui: &mut egui::Ui, name: &str, value: &str, color: egui::Color32) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [88.0, ui.spacing().interact_size.y],
            egui::Label::new(
                egui::RichText::new(name)
                    .size(theme::SIZE_SUB)
                    .color(theme::TEXT_DIM),
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
        ] {
            assert!(DevTab::ALL.contains(&tab), "{tab:?} is not in the tab row");
            assert!(!tab.label().is_empty());
        }
    }
}
